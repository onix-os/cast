//! Started -> Unverified durability classification for every journal seam.

use crate::{
    client::{boot, startup_recovery::DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord},
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
        CandidateOrigin, Epoch, UsrRestoreOrigin, assert_pending_phase, build_boot_sync_started, canonical_record,
        drive_boot_sync_started_to_candidate_preserved, enter_boot, enter_fresh_handles, expected_boot_repair_required,
        expected_boot_repair_unverified, install_persistent_boot_database, release_boot_handles,
        reset_boot_synchronize_observer, seed_boot_repair_started_for_test,
    },
};

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord::BootRepairStarted,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord::BootRepairStarted,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord::BootRepairUnverified,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord::BootRepairUnverified,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord::BootRepairUnverified,
    },
];

#[test]
fn startup_active_reblit_boot_repair_unverified_all_five_faults_converge_without_boot() {
    for fault in JOURNAL_FAULTS {
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
        let started = seed_boot_repair_started_for_test(&fixture, &required);
        let unverified = expected_boot_repair_unverified(&started);

        reset_boot_synchronize_observer();
        (fault.arm)();
        let _fault_entry = enter_boot(&fixture);

        (fault.assert_consumed)();
        assert_eq!(boot::boot_synchronize_attempt_count(), 0);
        assert_eq!(
            fixture.fixture.canonical_record(),
            match fault.durable {
                DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord::BootRepairStarted => started.clone(),
                DurableUsrRollbackActiveReblitBootRepairUnverifiedRecord::BootRepairUnverified => unverified.clone(),
            }
        );

        let retained = release_boot_handles(fixture);
        reset_boot_synchronize_observer();
        let fresh = enter_fresh_handles(retained.path());
        assert_pending_phase(&fresh, Phase::BootRepairUnverified);
        assert_eq!(canonical_record(retained.path()), unverified);
        assert_eq!(boot::boot_synchronize_attempt_count(), 0);
    }
}
