//! Operational namespace-capture faults remain hard startup errors.

use crate::{
    client::{
        boot, startup_gate,
        startup_reconciliation::{
            ActiveReblitBootRepairStartedCaptureFault, arm_active_reblit_boot_repair_started_capture_fault,
        },
    },
    transition_journal::Phase,
};

use super::{
    super::{Error as ActiveReblitDispatchError, test_fixture::BootSyncStartedLayout},
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, assert_pending_phase, build_boot_sync_started,
        drive_boot_sync_started_to_candidate_preserved, enter_boot, expected_boot_repair_required,
        install_persistent_boot_database, reset_boot_synchronize_observer, seed_boot_repair_started_for_test,
    },
};

const CAPTURE_FAULTS: [ActiveReblitBootRepairStartedCaptureFault; 4] = [
    ActiveReblitBootRepairStartedCaptureFault::PermissionDenied,
    ActiveReblitBootRepairStartedCaptureFault::Io,
    ActiveReblitBootRepairStartedCaptureFault::Timeout,
    ActiveReblitBootRepairStartedCaptureFault::RetryExhausted,
];

#[test]
fn startup_active_reblit_boot_repair_unverified_capture_faults_propagate_without_boot_or_journal_mutation() {
    for fault in CAPTURE_FAULTS {
        let mut fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
        install_persistent_boot_database(&mut fixture);
        let preserved = drive_boot_sync_started_to_candidate_preserved(
            &fixture,
            UsrRestoreOrigin::Applied,
            CandidateOrigin::Applied,
        );
        let required = expected_boot_repair_required(&preserved);
        let required_entry = enter_boot(&fixture);
        assert_pending_phase(&required_entry, Phase::BootRepairRequired);
        let started = seed_boot_repair_started_for_test(&fixture, &required);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();

        reset_boot_synchronize_observer();
        arm_active_reblit_boot_repair_started_capture_fault(fault);
        let error = enter_boot(&fixture);

        assert!(
            matches!(
                error,
                startup_gate::Error::UsrRollbackActiveReblitDispatch(
                    ActiveReblitDispatchError::BootRepairUnverifiedAuthority(_)
                )
            ),
            "operational {fault:?} capture failure was not propagated: {error:?}"
        );
        assert_eq!(fixture.fixture.canonical_record(), started);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_eq!(boot::boot_synchronize_attempt_count(), 0);
    }
}
