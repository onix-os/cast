//! Production error mapping for classified bound-delete outcomes.

use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            UsrRollbackActiveReblitFinalizationError,
            arm_after_usr_rollback_active_reblit_finalization_delete,
            finalize_usr_rollback_active_reblit,
        },
    },
    transition_journal::{
        TransitionJournalRecordDeleteError, TransitionJournalRecordDeleteState,
        arm_next_delete_canonical_unlink_fault, arm_next_delete_directory_sync_fault,
        assert_delete_canonical_unlink_fault_consumed, assert_delete_directory_sync_fault_consumed,
    },
};

use super::support::FinalizationFixture;

#[test]
fn active_reblit_finalization_preserves_exact_source_bound_delete_error() {
    let fixture = FinalizationFixture::new();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    arm_next_delete_canonical_unlink_fault();

    let error = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

    assert_delete_canonical_unlink_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackActiveReblitFinalizationError::Delete(
            TransitionJournalRecordDeleteError::Storage {
                state: TransitionJournalRecordDeleteState::ExactSource,
                ..
            }
        )
    ));
    assert_eq!(fixture.fixture.fixture.canonical_record(), fixture.terminal);
}

#[test]
fn active_reblit_finalization_preserves_absent_bound_delete_error() {
    let fixture = FinalizationFixture::new();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    arm_next_delete_directory_sync_fault();

    let error = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

    assert_delete_directory_sync_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackActiveReblitFinalizationError::Delete(
            TransitionJournalRecordDeleteError::Storage {
                state: TransitionJournalRecordDeleteState::Absent,
                ..
            }
        )
    ));
    assert!(
        !fixture
            .fixture
            .fixture
            .installation
            .root
            .join(".cast/journal/state-transition")
            .exists()
    );
}

#[test]
fn active_reblit_finalization_preserves_absent_error_when_post_delete_evidence_also_changes() {
    let fixture = FinalizationFixture::new();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let root_link = fixture.fixture.fixture.installation.root.join("bin");
    arm_next_delete_directory_sync_fault();
    arm_after_usr_rollback_active_reblit_finalization_delete(move || {
        fs::remove_file(root_link).unwrap();
    });

    let error = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

    assert_delete_directory_sync_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackActiveReblitFinalizationError::DeleteAndPostDeleteAuthority {
            delete: TransitionJournalRecordDeleteError::Storage {
                state: TransitionJournalRecordDeleteState::Absent,
                ..
            },
            ..
        }
    ));
}
