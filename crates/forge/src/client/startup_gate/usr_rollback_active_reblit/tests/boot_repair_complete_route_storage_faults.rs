//! All conditional journal-update faults with fresh-handle convergence.

use crate::{
    client::startup_recovery::DurableUsrRollbackActiveReblitBootRepairCompleteRecord,
    transition_journal::{
        BootRepairOutcome, Phase, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    super::test_fixture::BootSyncStartedLayout,
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, assert_boot_complete_persistence_advance,
        assert_fresh_existing_candidate_database, assert_no_boot_synchronize_attempts, assert_no_candidate_effects,
        assert_pending_phase, boot_active_wrapper_path, build_boot_sync_started, canonical_record,
        drive_boot_sync_started_to_candidate_preserved, enter_boot, enter_fresh_handles,
        expected_boot_repair_required, expected_boot_repair_rollback_complete, install_persistent_boot_database,
        release_boot_handles, reset_boot_synchronize_observer, reset_candidate_effect_observers,
        seed_boot_repair_complete_for_test,
    },
};

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableUsrRollbackActiveReblitBootRepairCompleteRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairCompleteRecord::BootRepairComplete,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairCompleteRecord::BootRepairComplete,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairCompleteRecord::RollbackComplete,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairCompleteRecord::RollbackComplete,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairCompleteRecord::RollbackComplete,
    },
];

#[test]
fn startup_active_reblit_boot_repair_complete_all_journal_faults_converge_without_finalization() {
    for fault in JOURNAL_FAULTS {
        let mut fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
        install_persistent_boot_database(&mut fixture);
        let preserved = drive_boot_sync_started_to_candidate_preserved(
            &fixture,
            UsrRestoreOrigin::Applied,
            CandidateOrigin::AlreadySatisfied,
        );
        let required = expected_boot_repair_required(&preserved);
        let required_entry = enter_boot(&fixture);
        assert_pending_phase(&required_entry, Phase::BootRepairRequired);
        let complete = seed_boot_repair_complete_for_test(&fixture, &required, BootRepairOutcome::Applied);
        let expected = expected_boot_repair_rollback_complete(&complete);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        let wrapper = boot_active_wrapper_path(&fixture);
        let provenance = fixture
            .fixture
            .database
            .metadata_provenance(fixture.fixture.candidate_state)
            .unwrap()
            .unwrap();
        reset_boot_synchronize_observer();
        reset_candidate_effect_observers();
        (fault.arm)();

        let first = enter_boot(&fixture);

        (fault.assert_consumed)();
        assert_boot_complete_persistence_advance(&first, fault.durable);
        assert_eq!(
            fixture.fixture.canonical_record(),
            match fault.durable {
                DurableUsrRollbackActiveReblitBootRepairCompleteRecord::BootRepairComplete => complete,
                DurableUsrRollbackActiveReblitBootRepairCompleteRecord::RollbackComplete => expected.clone(),
            }
        );
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert!(wrapper.join("usr").is_dir());
        assert_no_candidate_effects();
        assert_no_boot_synchronize_attempts();

        let retained = release_boot_handles(fixture);
        let second = enter_fresh_handles(retained.path());

        assert_pending_phase(&second, Phase::RollbackComplete);
        assert_eq!(canonical_record(retained.path()), expected);
        assert!(wrapper.join("usr").is_dir());
        assert_fresh_existing_candidate_database(retained.path(), &expected, &provenance);
        assert_no_candidate_effects();
        assert_no_boot_synchronize_attempts();
    }
}
