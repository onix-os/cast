//! Exact successful boot-repair matrix through rollback completion.

use crate::transition_journal::{BootRepairOutcome, Phase};

use super::{
    super::test_fixture::BootSyncStartedLayout,
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, assert_no_boot_synchronize_attempts, assert_no_candidate_effects,
        assert_pending_phase, boot_active_wrapper_path, build_boot_sync_started,
        drive_boot_sync_started_to_candidate_preserved, enter_boot, expected_boot_repair_required,
        expected_boot_repair_rollback_complete, reset_boot_synchronize_observer, reset_candidate_effect_observers,
        seed_boot_repair_complete_for_test,
    },
};

#[test]
fn startup_active_reblit_boot_repair_complete_routes_all_exact_success_outcomes_without_effects() {
    for epoch in Epoch::ALL {
        for usr_outcome in UsrRestoreOrigin::ALL {
            for candidate_outcome in CandidateOrigin::ALL {
                for boot_outcome in [BootRepairOutcome::Applied, BootRepairOutcome::AlreadySatisfied] {
                    let fixture = build_boot_sync_started(epoch, BootSyncStartedLayout::Post);
                    let preserved = drive_boot_sync_started_to_candidate_preserved(
                        &fixture,
                        usr_outcome,
                        candidate_outcome,
                    );
                    let required = expected_boot_repair_required(&preserved);
                    let required_entry = enter_boot(&fixture);
                    assert_pending_phase(&required_entry, Phase::BootRepairRequired);
                    let complete = seed_boot_repair_complete_for_test(&fixture, &required, boot_outcome);
                    let expected = expected_boot_repair_rollback_complete(&complete);
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = fixture.fixture.namespace_snapshot();
                    reset_boot_synchronize_observer();
                    reset_candidate_effect_observers();

                    let routed = enter_boot(&fixture);

                    assert_pending_phase(&routed, Phase::RollbackComplete);
                    assert_eq!(fixture.fixture.canonical_record(), expected);
                    assert_eq!(fixture.fixture.database_snapshot(), database_before);
                    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
                    assert!(boot_active_wrapper_path(&fixture).join("usr").is_dir());
                    assert_no_candidate_effects();
                    assert_no_boot_synchronize_attempts();

                    let retained = enter_boot(&fixture);

                    assert_pending_phase(&retained, Phase::RollbackComplete);
                    assert_eq!(fixture.fixture.canonical_record(), expected);
                    assert_eq!(fixture.fixture.database_snapshot(), database_before);
                    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
                    assert_no_candidate_effects();
                    assert_no_boot_synchronize_attempts();
                }
            }
        }
    }
}
