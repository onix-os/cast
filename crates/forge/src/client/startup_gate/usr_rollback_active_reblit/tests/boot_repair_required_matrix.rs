//! Genuine BootSyncStarted production ladder through the required boundary.

use crate::transition_journal::Phase;

use super::{
    super::test_fixture::BootSyncStartedLayout,
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, assert_no_boot_synchronize_attempts, assert_no_candidate_effects,
        assert_pending_phase, boot_active_wrapper_path, build_boot_sync_started,
        drive_boot_sync_started_to_candidate_preserved, enter_boot, expected_boot_repair_required,
        reset_boot_synchronize_observer, reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_boot_repair_required_covers_current_historical_applied_and_already_satisfied() {
    for epoch in Epoch::ALL {
        for usr_outcome in UsrRestoreOrigin::ALL {
            for candidate_outcome in CandidateOrigin::ALL {
                let fixture = build_boot_sync_started(epoch, BootSyncStartedLayout::Post);
                reset_boot_synchronize_observer();
                let preserved =
                    drive_boot_sync_started_to_candidate_preserved(&fixture, usr_outcome, candidate_outcome);
                let expected = expected_boot_repair_required(&preserved);
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = fixture.fixture.namespace_snapshot();
                reset_candidate_effect_observers();

                let error = enter_boot(&fixture);

                assert_pending_phase(&error, Phase::BootRepairRequired);
                assert_eq!(fixture.fixture.canonical_record(), expected);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
                assert!(boot_active_wrapper_path(&fixture).join("usr").is_dir());
                assert_no_candidate_effects();
                assert_no_boot_synchronize_attempts();
            }
        }
    }
}
