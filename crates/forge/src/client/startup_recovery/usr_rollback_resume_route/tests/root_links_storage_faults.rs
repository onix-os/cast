use std::fs;

use crate::{
    client::{
        startup_gate,
        startup_recovery::{DurableUsrRollbackResumeRouteRecord, UsrRollbackResumeRoutePersistenceError},
    },
    transition_journal::{
        arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault, arm_next_update_exchange_fault,
        arm_next_update_final_directory_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_displaced_unlink_fault_consumed, assert_temporary_sync_fault_consumed,
        assert_update_exchange_fault_consumed, assert_update_final_directory_sync_fault_consumed,
        assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    fixture::OperationKind,
    support::RouteFixture,
};

#[test]
fn startup_root_links_complete_route_all_storage_faults_reopen_exact_record_across_operations_and_epochs() {
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

    for historical in [false, true] {
        for kind in OperationKind::ALL {
            for &(arm, assert_consumed, expected_durable) in &cases {
                for fixture in RouteFixture::root_links_routes_at_epoch(kind, historical) {
                    let phase = fixture.source.phase;
                    let usr_outcome = fixture.source.rollback.as_ref().unwrap().usr_exchange;
                    arm();

                    let error = fixture.enter();

                    assert_consumed();
                    assert!(
                        matches!(
                            error,
                            startup_gate::Error::UsrRollbackResumeRoutePersistence(
                                UsrRollbackResumeRoutePersistenceError::Advance { durable, .. }
                            ) if durable == expected_durable
                        ),
                        "{kind:?} {phase:?} {usr_outcome:?} historical={historical} durable={expected_durable:?}: {error:?}"
                    );
                    let actual = fixture.canonical_record();
                    match expected_durable {
                        DurableUsrRollbackResumeRouteRecord::Source => {
                            assert_eq!(actual, fixture.source, "{kind:?} {phase:?} historical={historical}")
                        }
                        DurableUsrRollbackResumeRouteRecord::Successor => fixture.assert_exact_route(&actual),
                    }
                    let names = fs::read_dir(fixture.fixture.installation.root.join(".cast/journal"))
                        .unwrap()
                        .map(|entry| entry.unwrap().file_name())
                        .collect::<Vec<_>>();
                    assert_eq!(
                        names.len(),
                        2,
                        "{kind:?} {phase:?} historical={historical}: stale journal residue remained after reopen: {names:?}"
                    );
                }
            }
        }
    }
}
