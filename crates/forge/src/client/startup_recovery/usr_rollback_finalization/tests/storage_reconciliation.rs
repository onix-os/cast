//! Bound-delete storage classification and same-lock handoff contracts.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{UsrRollbackFinalizationError, finalize_usr_rollback},
    },
    transition_journal::{
        RollbackActionOutcome, TransitionJournalRecordDeleteError, TransitionJournalRecordDeleteState,
        TransitionJournalStore, arm_next_delete_canonical_unlink_fault, arm_next_delete_directory_sync_fault,
        assert_delete_canonical_unlink_fault_consumed, assert_delete_directory_sync_fault_consumed,
    },
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source};

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
fn startup_usr_rollback_finalization_storage_matrix_preserves_all_bound_delete_errors() {
    let mut cases = 0;
    for source in Source::THROUGH_ROLLBACK_COMPLETE {
        for fault in DELETE_FAULTS {
            let fixture = FinalizationFixture::new(
                FreshDbOutcome::Applied,
                source,
                RollbackActionOutcome::Applied,
                CandidateResult::Applied,
            );
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = fixture.capture_ready(&journal, &reservation);
            let database_before = fixture.database_snapshot();
            let namespace_before = fixture.namespace_snapshot();
            (fault.arm)();

            let error = finalize_usr_rollback(journal, authority).unwrap_err();

            (fault.assert_consumed)();
            assert!(
                matches!(
                    error,
                    UsrRollbackFinalizationError::Delete(TransitionJournalRecordDeleteError::Storage {
                        state,
                        ..
                    }) if state == fault.state
                ),
                "source={source:?}, state={:?}: {error:?}",
                fault.state
            );
            match fault.state {
                TransitionJournalRecordDeleteState::ExactSource => {
                    assert_eq!(fixture.canonical_record(), fixture.source);
                }
                TransitionJournalRecordDeleteState::Absent => {
                    assert!(
                        !fixture
                            .installation()
                            .root
                            .join(".cast/journal/state-transition")
                            .exists()
                    );
                }
            }
            assert_eq!(fixture.database_snapshot(), database_before);
            assert_eq!(fixture.namespace_snapshot(), namespace_before);
            fixture.assert_no_second_removal();
            cases += 1;
        }
    }
    assert_eq!(cases, 6);
}

#[test]
fn startup_usr_rollback_finalization_returns_the_same_continuously_locked_store() {
    for origin in FreshDbOutcome::ALL {
        let fixture = FinalizationFixture::historical(
            origin,
            Source::RootLinksComplete,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateResult::AlreadySatisfied,
        );
        let journal = fixture.open_journal();
        let binding = journal.binding();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);

        let retained = finalize_usr_rollback(journal, authority).unwrap();

        assert!(retained.has_binding(&binding));
        assert_eq!(retained.load().unwrap(), None);
        let cast = fixture.installation().retained_mutable_cast_directory().unwrap();
        let error = TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.installation().root).unwrap_err();
        assert!(matches!(error, crate::transition_journal::StorageError::AcquireLock { .. }));
        drop(retained);
        let independent =
            TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.installation().root).unwrap();
        assert_eq!(independent.load().unwrap(), None);
        fixture.assert_no_second_removal();
    }
}
