//! Defensive branches for delete reports that the real syscall path cannot deterministically produce.

use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackActivateArchivedFinalizationRecord, UsrRollbackActivateArchivedFinalizationError,
            UsrRollbackActivateArchivedFinalizationVerificationError,
            arm_after_usr_rollback_activate_archived_finalization_delete, finalize_usr_rollback_activate_archived,
        },
    },
    transition_journal::{
        arm_next_delete_canonical_unlink_fault, assert_delete_canonical_unlink_fault_consumed, encode,
    },
};

use super::{super::reconcile_delete, support::FinalizationFixture, test_fixture::canonical_journal};

#[test]
fn activate_archived_finalization_false_delete_report_classifies_exact_source() {
    let fixture = FinalizationFixture::new();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let installation = fixture.fixture.fixture.installation.clone();

    let error = reconcile_delete(Ok(false), journal, authority, &installation, fixture.terminal.clone()).unwrap_err();

    assert!(matches!(
        error,
        UsrRollbackActivateArchivedFinalizationError::DeleteReportedFalse {
            durable: DurableUsrRollbackActivateArchivedFinalizationRecord::RollbackComplete,
        }
    ));
    assert_eq!(fixture.fixture.fixture.canonical_record(), fixture.terminal);
}

#[test]
fn activate_archived_finalization_false_delete_report_classifies_authenticated_absence() {
    let fixture = FinalizationFixture::new();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let installation = fixture.fixture.fixture.installation.clone();
    let canonical = canonical_journal(&installation.root);
    let journal_directory = fs::File::open(canonical.parent().unwrap()).unwrap();
    fs::remove_file(&canonical).unwrap();
    journal_directory.sync_all().unwrap();

    let error = reconcile_delete(Ok(false), journal, authority, &installation, fixture.terminal.clone()).unwrap_err();

    assert!(matches!(
        error,
        UsrRollbackActivateArchivedFinalizationError::DeleteReportedFalse {
            durable: DurableUsrRollbackActivateArchivedFinalizationRecord::Absent,
        }
    ));
    assert!(!canonical_journal(&installation.root).exists());
}

#[test]
fn activate_archived_finalization_false_delete_report_rejects_an_unexpected_record() {
    let fixture = FinalizationFixture::new();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let installation = fixture.fixture.fixture.installation.clone();
    fs::write(
        canonical_journal(&installation.root),
        encode(&fixture.preterminal).unwrap(),
    )
    .unwrap();

    let error = reconcile_delete(Ok(false), journal, authority, &installation, fixture.terminal.clone()).unwrap_err();

    assert!(matches!(
        error,
        UsrRollbackActivateArchivedFinalizationError::DeleteReportedFalseAndVerification {
            source: UsrRollbackActivateArchivedFinalizationVerificationError::UnexpectedRecord { actual: Some(_), .. },
        }
    ));
    assert_eq!(fixture.fixture.fixture.canonical_record(), fixture.preterminal);
}

#[test]
fn activate_archived_finalization_delete_error_preserves_ambiguous_verification() {
    let fixture = FinalizationFixture::new();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let canonical = canonical_journal(&fixture.fixture.fixture.installation.root);
    let unexpected = fixture.preterminal.clone();
    let encoded = encode(&unexpected).unwrap();
    arm_next_delete_canonical_unlink_fault();
    arm_after_usr_rollback_activate_archived_finalization_delete(move || fs::write(canonical, encoded).unwrap());

    let error = finalize_usr_rollback_activate_archived(journal, authority).unwrap_err();

    assert_delete_canonical_unlink_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackActivateArchivedFinalizationError::DeleteAndVerification {
            verification: UsrRollbackActivateArchivedFinalizationVerificationError::UnexpectedRecord {
                actual: Some(_),
                ..
            },
            ..
        }
    ));
    assert_eq!(fixture.fixture.fixture.canonical_record(), unexpected);
}
