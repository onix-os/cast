use std::{fs, os::unix::fs::MetadataExt as _};

use crate::{
    client::{
        startup_gate,
        startup_reconciliation::{
            reset_usr_exchanged_root_abi_effect_counts, usr_exchanged_root_abi_complete_sync_attempts,
            usr_exchanged_root_abi_publication_attempts,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
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
fn startup_root_links_complete_next_entry_routes_exact_decision_without_reverse_effect() {
    for kind in OperationKind::ALL {
        let fixture = Fixture::new(kind, SourceCase::RootLinksCompletePost);
        reset_usr_exchanged_root_abi_effect_counts();
        reset_retained_exchange_syscall_count();
        let first = fixture.enter();
        assert_eq!(pending(&first).phase(), Phase::RollbackDecided, "{kind:?}");
        drop(first);
        let decision = fixture.canonical_record();
        fixture.assert_exact_decision(&decision);
        let reverse_intent = decision.rollback_successor(None).unwrap();
        assert_eq!(reverse_intent.phase, Phase::ReverseExchangeIntent, "{kind:?}");
        let database_before = fixture.database_snapshot();
        let usr_before = directory_identity(&fixture.installation.root.join("usr"));

        let second = fixture.enter();
        assert_eq!(pending(&second).phase(), Phase::ReverseExchangeIntent, "{kind:?}");
        assert_eq!(
            pending(&second).disposition(),
            RecoveryDisposition::ResumeRollback {
                phase: Phase::ReverseExchangeIntent,
            },
            "{kind:?}"
        );
        assert!(pending(&second).blockers().is_empty(), "{kind:?}");
        assert_eq!(fixture.canonical_record(), reverse_intent, "{kind:?}");
        assert_eq!(fixture.database_snapshot(), database_before, "{kind:?}");
        assert_eq!(directory_identity(&fixture.installation.root.join("usr")), usr_before, "{kind:?}");
        assert_eq!(retained_exchange_syscall_count(), 0, "{kind:?}");
        assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0, "{kind:?}");
        assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 0, "{kind:?}");
        fixture.assert_complete_root_abi();
    }
}

fn directory_identity(path: &std::path::Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir());
    (metadata.dev(), metadata.ino())
}

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

    for (kind, source) in [
        (OperationKind::NewState, SourceCase::IntentPre),
        (OperationKind::NewState, SourceCase::RootLinksCompletePost),
        (OperationKind::Archived, SourceCase::RootLinksCompletePost),
        (OperationKind::ActiveReblit, SourceCase::RootLinksCompletePost),
    ] {
        for (arm, assert_consumed, expected_durable) in cases {
            let fixture = Fixture::new(kind, source);
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
            assert_eq!(
                names.len(),
                2,
                "stale journal residue remained after reopen for {kind:?} {source:?}: {names:?}"
            );
        }
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
fn startup_usr_rollback_decision_next_startup_routes_exact_decision() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    let first = fixture.enter();
    assert_eq!(pending(&first).phase(), Phase::RollbackDecided);
    drop(first);
    let decision = fixture.canonical_record();
    fixture.assert_exact_decision(&decision);

    let second = fixture.enter();
    let expected_route = decision.rollback_successor(None).unwrap();
    assert_eq!(pending(&second).phase(), Phase::ReverseExchangeIntent);
    assert_eq!(fixture.canonical_record(), expected_route);
}
