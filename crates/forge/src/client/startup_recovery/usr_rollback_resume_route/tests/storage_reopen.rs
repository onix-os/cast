use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackResumeRouteRecord, UsrRollbackResumeRoutePersistenceError,
            persist_usr_rollback_resume_route_and_reopen,
        },
    },
    transition_journal::{
        Phase, TransitionJournalStore, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    fixture::{OperationKind, SourceCase, canonical_journal, pending},
    support::RouteFixture,
};

#[test]
fn startup_usr_rollback_resume_route_storage_faults_reopen_to_exact_source_or_successor() {
    let cases: [(fn(), fn(), DurableUsrRollbackResumeRouteRecord); 5] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableUsrRollbackResumeRouteRecord::Source,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableUsrRollbackResumeRouteRecord::Source,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableUsrRollbackResumeRouteRecord::Successor,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableUsrRollbackResumeRouteRecord::Successor,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableUsrRollbackResumeRouteRecord::Successor,
        ),
    ];

    for (arm, assert_consumed, expected_durable) in cases {
        let fixture = RouteFixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
        arm();
        let error = fixture.enter();
        assert_consumed();
        assert!(matches!(
            error,
            crate::client::startup_gate::Error::UsrRollbackResumeRoutePersistence(
                UsrRollbackResumeRoutePersistenceError::Advance { durable, .. }
            ) if durable == expected_durable
        ));
        let actual = fixture.canonical_record();
        match expected_durable {
            DurableUsrRollbackResumeRouteRecord::Source => assert_eq!(actual, fixture.decision),
            DurableUsrRollbackResumeRouteRecord::Successor => fixture.assert_exact_route(&actual),
        }
        let names = fs::read_dir(fixture.fixture.installation.root.join(".cast/journal"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 2, "stale journal residue remained after reopen: {names:?}");
    }
}

#[test]
fn startup_usr_rollback_resume_route_rejects_cross_root_authority_and_reopens_success() {
    let first = RouteFixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let second = RouteFixture::new(OperationKind::Archived, SourceCase::IntentPre);
    fs::write(
        canonical_journal(&second.fixture.installation.root),
        first.fixture.canonical_bytes(),
    )
    .unwrap();
    let first_journal = first.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = first.capture_ready(&first_journal, &reservation);
    let second_journal = TransitionJournalStore::open_retained(
        second.fixture.installation.root_directory(),
        &second.fixture.installation.root,
    )
    .unwrap();
    let error = persist_usr_rollback_resume_route_and_reopen(second_journal, authority).unwrap_err();
    assert!(matches!(error, UsrRollbackResumeRoutePersistenceError::Authority(_)));
    assert_eq!(first_journal.load().unwrap(), Some(first.decision.clone()));
    assert_eq!(first.canonical_record(), first.decision);
    assert_eq!(second.canonical_record(), first.decision);

    drop(first_journal);
    let journal = first.open_journal();
    let authority = first.capture_ready(&journal, &reservation);
    let (reopened, actual) = persist_usr_rollback_resume_route_and_reopen(journal, authority).unwrap();
    first.assert_exact_route(&actual);
    assert_eq!(reopened.load().unwrap(), Some(actual.clone()));
    drop(reopened);
    drop(reservation);

    let retry = first.enter();
    assert_eq!(pending(&retry).phase(), Phase::CandidatePreserveIntent);
    assert_eq!(first.canonical_record(), actual);
}
