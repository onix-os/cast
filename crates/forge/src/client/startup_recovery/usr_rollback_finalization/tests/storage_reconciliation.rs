//! Terminal deletion durability and same-store reconciliation contracts.

use std::{
    fs,
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackFinalizationRecord, UsrRollbackFinalizationError,
            UsrRollbackFinalizationVerificationError, arm_after_usr_rollback_finalization_delete,
            arm_before_usr_rollback_finalization_final_durable_inspection, finalize_usr_rollback,
        },
    },
    transition_journal::{
        RollbackActionOutcome, TransitionJournalStore, arm_next_delete_canonical_unlink_fault,
        arm_next_delete_directory_sync_fault, assert_delete_canonical_unlink_fault_consumed,
        assert_delete_directory_sync_fault_consumed, encode,
    },
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source, canonical_journal};

#[test]
fn startup_usr_rollback_finalization_delete_faults_observe_exact_terminal_or_absence() {
    let cases: [(fn(), fn(), DurableUsrRollbackFinalizationRecord); 2] = [
        (
            arm_next_delete_canonical_unlink_fault,
            assert_delete_canonical_unlink_fault_consumed,
            DurableUsrRollbackFinalizationRecord::RollbackComplete,
        ),
        (
            arm_next_delete_directory_sync_fault,
            assert_delete_directory_sync_fault_consumed,
            DurableUsrRollbackFinalizationRecord::Absent,
        ),
    ];

    for (arm, assert_consumed, expected_durable) in cases {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::Applied,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::Applied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        arm();

        let error = finalize_usr_rollback(journal, authority).unwrap_err();

        assert_consumed();
        assert!(
            matches!(
                error,
                UsrRollbackFinalizationError::Delete { durable, .. }
                    if durable == expected_durable
            ),
            "{expected_durable:?}: {error:?}"
        );
        let observed = fixture.open_journal();
        assert_eq!(
            observed.load().unwrap(),
            match expected_durable {
                DurableUsrRollbackFinalizationRecord::RollbackComplete => Some(fixture.source.clone()),
                DurableUsrRollbackFinalizationRecord::Absent => None,
            }
        );
        assert_eq!(fixture.database_snapshot(), database_before);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
        fixture.assert_no_second_removal();
    }
}

#[test]
fn startup_usr_rollback_finalization_returns_the_same_continuously_locked_store() {
    for origin in FreshDbOutcome::ALL {
        let fixture = FinalizationFixture::historical(
            origin,
            Source::Intent,
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
        drop(retained);
        let cast = fixture.installation().retained_mutable_cast_directory().unwrap();
        let independent =
            TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.installation().root).unwrap();
        assert_eq!(independent.load().unwrap(), None);
        fixture.assert_no_second_removal();
    }
}

#[test]
fn startup_usr_rollback_finalization_rejects_records_recreated_after_delete() {
    for exact_source in [true, false] {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::Applied,
            Source::Exchanged,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateResult::Applied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let canonical = canonical_journal(&fixture.installation().root);
        let recreated = if exact_source {
            fixture.source.clone()
        } else {
            fixture.preterminal_record().clone()
        };
        let encoded = encode(&recreated).unwrap();
        arm_after_usr_rollback_finalization_delete(move || write_new_private_record(&canonical, &encoded));

        let error = finalize_usr_rollback(journal, authority).unwrap_err();

        if exact_source {
            assert!(matches!(
                error,
                UsrRollbackFinalizationError::DeleteSucceededButRecordPresent
            ));
        } else {
            assert!(matches!(
                error,
                UsrRollbackFinalizationError::PostDeleteVerification(
                    UsrRollbackFinalizationVerificationError::UnexpectedRecord { actual: Some(_), .. }
                )
            ));
        }
        assert_eq!(fixture.canonical_record(), recreated);
        fixture.assert_no_second_removal();
    }
}

#[test]
fn startup_usr_rollback_finalization_reports_delete_error_with_ambiguous_observation() {
    let fixture = FinalizationFixture::historical(
        FreshDbOutcome::AlreadySatisfied,
        Source::Intent,
        RollbackActionOutcome::Applied,
        CandidateResult::Applied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let canonical = canonical_journal(&fixture.installation().root);
    let unexpected = fixture.preterminal_record().clone();
    let encoded = encode(&unexpected).unwrap();
    arm_next_delete_canonical_unlink_fault();
    arm_after_usr_rollback_finalization_delete(move || fs::write(canonical, encoded).unwrap());

    let error = finalize_usr_rollback(journal, authority).unwrap_err();

    assert_delete_canonical_unlink_fault_consumed();
    assert!(matches!(
        error,
        UsrRollbackFinalizationError::DeleteAndVerification {
            verification: UsrRollbackFinalizationVerificationError::UnexpectedRecord { actual: Some(_), .. },
            ..
        }
    ));
    assert_eq!(fixture.canonical_record(), unexpected);
    fixture.assert_no_second_removal();
}

#[test]
fn startup_usr_rollback_finalization_rejects_change_after_consuming_post_delete_authority() {
    let fixture = FinalizationFixture::new(
        FreshDbOutcome::Applied,
        Source::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateResult::Applied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let canonical = canonical_journal(&fixture.installation().root);
    let source = fixture.source.clone();
    let encoded = encode(&source).unwrap();
    arm_before_usr_rollback_finalization_final_durable_inspection(move || {
        write_new_private_record(&canonical, &encoded)
    });

    let error = finalize_usr_rollback(journal, authority).unwrap_err();

    assert!(matches!(
        error,
        UsrRollbackFinalizationError::PostDeleteVerification(
            UsrRollbackFinalizationVerificationError::JournalChangedDuringVerification {
                before: DurableUsrRollbackFinalizationRecord::Absent,
                after: DurableUsrRollbackFinalizationRecord::RollbackComplete,
            }
        )
    ));
    assert_eq!(fixture.canonical_record(), source);
    fixture.assert_no_second_removal();
}

fn write_new_private_record(path: &std::path::Path, encoded: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o600)).unwrap();
    file.write_all(encoded).unwrap();
    file.sync_all().unwrap();
}
