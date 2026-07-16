use std::fs;

use crate::{
    client::startup_gate,
    transition_journal::{
        Phase, RecoveryDisposition, TransitionJournalStore, arm_next_displaced_unlink_fault,
        arm_next_temporary_sync_fault, arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    super::{DurableUsrRollbackDecisionRecord, UsrRollbackDecisionPersistenceError},
    fixture::{Fixture, OperationKind, SourceCase, pending},
};

#[test]
fn startup_usr_rollback_decision_storage_faults_reopen_to_exact_source_or_decision() {
    let cases: [(fn(), fn(), DurableUsrRollbackDecisionRecord); 5] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableUsrRollbackDecisionRecord::Source,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableUsrRollbackDecisionRecord::Source,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableUsrRollbackDecisionRecord::Decision,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableUsrRollbackDecisionRecord::Decision,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableUsrRollbackDecisionRecord::Decision,
        ),
    ];

    for (arm, assert_consumed, expected_durable) in cases {
        let fixture = Fixture::new(OperationKind::NewState, SourceCase::IntentPre);
        arm();
        let error = fixture.enter();
        assert_consumed();
        assert!(matches!(
            error,
            startup_gate::Error::UsrRollbackDecisionPersistence(
                UsrRollbackDecisionPersistenceError::Advance { durable, .. }
            ) if durable == expected_durable
        ));
        let actual = fixture.canonical_record();
        match expected_durable {
            DurableUsrRollbackDecisionRecord::Source => assert_eq!(actual, fixture.source),
            DurableUsrRollbackDecisionRecord::Decision => fixture.assert_exact_decision(&actual),
        }
        let names = fs::read_dir(fixture.installation.root.join(".cast/journal"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 2, "stale journal residue remained after reopen: {names:?}");
    }
}

#[test]
fn startup_usr_rollback_decision_consumes_journal_before_reopen() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let error = fixture.enter();
    let pending = pending(&error);
    assert_eq!(pending.phase(), Phase::RollbackDecided);
    assert_eq!(
        pending.disposition(),
        RecoveryDisposition::ResumeRollback {
            phase: Phase::RollbackDecided,
        }
    );

    let cast = fixture.installation.retained_mutable_cast_directory().unwrap();
    let independently_opened =
        TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.installation.root).unwrap();
    let actual = independently_opened.load().unwrap().unwrap();
    fixture.assert_exact_decision(&actual);
}

#[test]
fn startup_usr_rollback_decision_retry_stops_at_rollback_decided() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    let first = fixture.enter();
    assert_eq!(pending(&first).phase(), Phase::RollbackDecided);
    drop(first);
    let decision = fixture.canonical_record();
    fixture.assert_exact_decision(&decision);

    let second = fixture.enter();
    assert_eq!(pending(&second).phase(), Phase::RollbackDecided);
    assert_eq!(fixture.canonical_record(), decision);
}
