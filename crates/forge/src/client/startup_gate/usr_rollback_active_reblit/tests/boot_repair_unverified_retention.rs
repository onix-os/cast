//! Manual-recovery retention after the conservative journal-only route.

use crate::{
    client::{boot, startup_gate, startup_reconciliation::RecoveryBlocker},
    transition_journal::{Phase, RecoveryDisposition},
};

use super::{
    super::test_fixture::BootSyncStartedLayout,
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, assert_no_candidate_effects, assert_pending_phase,
        build_boot_sync_started, drive_boot_sync_started_to_candidate_preserved, enter_boot,
        expected_boot_repair_required, expected_boot_repair_unverified, install_persistent_boot_database,
        reset_boot_synchronize_observer, reset_candidate_effect_observers, seed_boot_repair_started_for_test,
    },
};

#[test]
fn startup_active_reblit_boot_repair_unverified_is_retained_exactly_for_manual_recovery() {
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
    let started = seed_boot_repair_started_for_test(&fixture, &required);
    let unverified = expected_boot_repair_unverified(&started);

    reset_boot_synchronize_observer();
    let conservative = enter_boot(&fixture);
    assert_pending_phase(&conservative, Phase::BootRepairUnverified);
    assert_eq!(fixture.fixture.canonical_record(), unverified);
    assert_eq!(boot::boot_synchronize_attempt_count(), 0);

    let exact_bytes = fixture.fixture.canonical_bytes();
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_boot_synchronize_observer();
    reset_candidate_effect_observers();

    for _ in 0..3 {
        let error = enter_boot(&fixture);
        let startup_gate::Error::RecoveryPending(pending) = error else {
            panic!("retained BootRepairUnverified did not remain a structured pending transition")
        };
        assert_eq!(pending.phase(), Phase::BootRepairUnverified);
        assert_eq!(pending.disposition(), RecoveryDisposition::ManualBootRepair);
        assert!(pending.blockers().contains(&RecoveryBlocker::ManualBootRepair));
        assert_eq!(fixture.fixture.canonical_bytes(), exact_bytes);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_eq!(boot::boot_synchronize_attempt_count(), 0);
        assert_no_candidate_effects();
    }
}
