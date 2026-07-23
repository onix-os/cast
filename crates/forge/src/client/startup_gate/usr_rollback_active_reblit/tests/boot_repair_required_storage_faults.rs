//! All five conditional journal-update faults with fresh-handle convergence.

use crate::{
    client::startup_recovery::{
        DurableUsrRollbackActiveReblitBootRepairRequiredRecord,
        DurableUsrRollbackActiveReblitBootRepairStartRecord,
    },
    transition_journal::{
        Phase, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault, arm_next_update_exchange_fault,
        arm_next_update_final_directory_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_displaced_unlink_fault_consumed, assert_temporary_sync_fault_consumed,
        assert_update_exchange_fault_consumed, assert_update_final_directory_sync_fault_consumed,
        assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    super::test_fixture::BootSyncStartedLayout,
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, assert_boot_required_persistence_advance,
        assert_boot_start_persistence_advance,
        assert_fresh_existing_candidate_database, assert_no_boot_synchronize_attempts, assert_no_candidate_effects,
        assert_pending_phase, boot_active_wrapper_path, build_boot_sync_started, canonical_record,
        drive_boot_sync_started_to_candidate_preserved, enter_boot, enter_fresh_handles, expected_boot_repair_required,
        expected_boot_repair_started, expected_boot_repair_unverified,
        install_persistent_boot_database, release_boot_handles, reset_boot_synchronize_observer,
        reset_candidate_effect_observers,
    },
};

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableUsrRollbackActiveReblitBootRepairRequiredRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairRequiredRecord::CandidatePreserved,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairRequiredRecord::CandidatePreserved,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairRequiredRecord::BootRepairRequired,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairRequiredRecord::BootRepairRequired,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairRequiredRecord::BootRepairRequired,
    },
];

#[derive(Clone, Copy)]
struct StartJournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableUsrRollbackActiveReblitBootRepairStartRecord,
}

const START_JOURNAL_FAULTS: [StartJournalFault; 5] = [
    StartJournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairRequired,
    },
    StartJournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairRequired,
    },
    StartJournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
    },
    StartJournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
    },
    StartJournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted,
    },
];

#[test]
fn startup_active_reblit_boot_repair_required_all_five_journal_faults_converge_fresh_without_boot() {
    for fault in JOURNAL_FAULTS {
        let mut fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
        install_persistent_boot_database(&mut fixture);
        reset_boot_synchronize_observer();
        let source = drive_boot_sync_started_to_candidate_preserved(
            &fixture,
            UsrRestoreOrigin::Applied,
            CandidateOrigin::AlreadySatisfied,
        );
        let expected = expected_boot_repair_required(&source);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        let wrapper = boot_active_wrapper_path(&fixture);
        let provenance = fixture
            .fixture
            .database
            .metadata_provenance(fixture.fixture.candidate_state)
            .unwrap()
            .unwrap();
        reset_candidate_effect_observers();
        (fault.arm)();

        let first = enter_boot(&fixture);

        (fault.assert_consumed)();
        assert_boot_required_persistence_advance(&first, fault.durable);
        assert_eq!(
            fixture.fixture.canonical_record(),
            match fault.durable {
                DurableUsrRollbackActiveReblitBootRepairRequiredRecord::CandidatePreserved => source,
                DurableUsrRollbackActiveReblitBootRepairRequiredRecord::BootRepairRequired => expected.clone(),
            }
        );
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert!(wrapper.join("usr").is_dir());
        assert_no_candidate_effects();
        assert_no_boot_synchronize_attempts();

        let retained = release_boot_handles(fixture);
        let second = enter_fresh_handles(retained.path());

        let started = expected_boot_repair_started(&expected);
        let (phase, durable_record) = match fault.durable {
            DurableUsrRollbackActiveReblitBootRepairRequiredRecord::CandidatePreserved => {
                (Phase::BootRepairRequired, expected)
            }
            DurableUsrRollbackActiveReblitBootRepairRequiredRecord::BootRepairRequired => {
                (Phase::BootRepairStarted, started)
            }
        };
        assert_pending_phase(&second, phase);
        assert_eq!(canonical_record(retained.path()), durable_record);
        assert!(wrapper.join("usr").is_dir());
        assert_fresh_existing_candidate_database(retained.path(), &durable_record, &provenance);
        assert_no_candidate_effects();
        assert_no_boot_synchronize_attempts();
    }

    for fault in START_JOURNAL_FAULTS {
        let mut fixture = build_boot_sync_started(Epoch::Historical, BootSyncStartedLayout::Post);
        install_persistent_boot_database(&mut fixture);
        let preserved = drive_boot_sync_started_to_candidate_preserved(
            &fixture,
            UsrRestoreOrigin::AlreadySatisfied,
            CandidateOrigin::Applied,
        );
        let required = expected_boot_repair_required(&preserved);
        let required_entry = enter_boot(&fixture);
        assert_pending_phase(&required_entry, Phase::BootRepairRequired);
        let started = expected_boot_repair_started(&required);
        let unverified = expected_boot_repair_unverified(&started);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        let wrapper = boot_active_wrapper_path(&fixture);
        let provenance = fixture
            .fixture
            .database
            .metadata_provenance(fixture.fixture.candidate_state)
            .unwrap()
            .unwrap();
        reset_candidate_effect_observers();
        reset_boot_synchronize_observer();
        (fault.arm)();

        let first = enter_boot(&fixture);

        (fault.assert_consumed)();
        assert_boot_start_persistence_advance(&first, fault.durable);
        let durable_record = match fault.durable {
            DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairRequired => required.clone(),
            DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted => started.clone(),
        };
        assert_eq!(fixture.fixture.canonical_record(), durable_record);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert!(wrapper.join("usr").is_dir());
        assert_no_candidate_effects();
        assert_no_boot_synchronize_attempts();

        let retained = release_boot_handles(fixture);
        let second = enter_fresh_handles(retained.path());
        let (phase, expected) = match fault.durable {
            DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairRequired => {
                (Phase::BootRepairStarted, started)
            }
            DurableUsrRollbackActiveReblitBootRepairStartRecord::BootRepairStarted => {
                (Phase::BootRepairUnverified, unverified)
            }
        };
        assert_pending_phase(&second, phase);
        assert_eq!(canonical_record(retained.path()), expected);
        assert!(wrapper.join("usr").is_dir());
        assert_fresh_existing_candidate_database(retained.path(), &expected, &provenance);
        assert_no_candidate_effects();
        assert_no_boot_synchronize_attempts();
    }
}
