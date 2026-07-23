//! Production error mapping for classified NewState bound-delete outcomes.

use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            UsrRollbackFinalizationError, arm_after_usr_rollback_finalization_delete,
            finalize_usr_rollback,
        },
    },
    transition_journal::{
        RollbackActionOutcome, TransitionJournalRecordDeleteError, TransitionJournalRecordDeleteState,
        arm_next_delete_canonical_unlink_fault, arm_next_delete_directory_sync_fault,
        assert_delete_canonical_unlink_fault_consumed, assert_delete_directory_sync_fault_consumed,
    },
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source};

#[test]
fn new_state_finalization_preserves_exact_source_bound_delete_error() {
    let fixture = FinalizationFixture::new(
        FreshDbOutcome::Applied,
        Source::RootLinksComplete,
        RollbackActionOutcome::Applied,
        CandidateResult::Applied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    arm_next_delete_canonical_unlink_fault();

    let error = finalize_usr_rollback(journal, authority).unwrap_err();

    assert_delete_canonical_unlink_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackFinalizationError::Delete(TransitionJournalRecordDeleteError::Storage {
            state: TransitionJournalRecordDeleteState::ExactSource,
            ..
        })
    ));
    assert_eq!(fixture.canonical_record(), fixture.source);
}

#[test]
fn new_state_finalization_preserves_absent_bound_delete_error() {
    let fixture = FinalizationFixture::new(
        FreshDbOutcome::AlreadySatisfied,
        Source::RootLinksComplete,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateResult::AlreadySatisfied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    arm_next_delete_directory_sync_fault();

    let error = finalize_usr_rollback(journal, authority).unwrap_err();

    assert_delete_directory_sync_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackFinalizationError::Delete(TransitionJournalRecordDeleteError::Storage {
            state: TransitionJournalRecordDeleteState::Absent,
            ..
        })
    ));
    assert!(
        !fixture
            .installation()
            .root
            .join(".cast/journal/state-transition")
            .exists()
    );
}

#[test]
fn new_state_finalization_preserves_absent_error_when_post_delete_evidence_also_changes() {
    let fixture = FinalizationFixture::new(
        FreshDbOutcome::Applied,
        Source::RootLinksComplete,
        RollbackActionOutcome::Applied,
        CandidateResult::AlreadySatisfied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let root_link = fixture.installation().root.join("bin");
    arm_next_delete_directory_sync_fault();
    arm_after_usr_rollback_finalization_delete(move || {
        fs::remove_file(root_link).unwrap();
    });

    let error = finalize_usr_rollback(journal, authority).unwrap_err();

    assert_delete_directory_sync_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackFinalizationError::DeleteAndPostDeleteAuthority {
            delete: TransitionJournalRecordDeleteError::Storage {
                state: TransitionJournalRecordDeleteState::Absent,
                ..
            },
            ..
        }
    ));
}
