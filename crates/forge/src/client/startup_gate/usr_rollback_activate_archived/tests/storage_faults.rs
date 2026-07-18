//! All five conditional journal-update faults at the test-sealed route.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackActivateArchivedCompleteRouteRecord,
            UsrRollbackActivateArchivedCompleteRoutePersistenceError,
            persist_usr_rollback_activate_archived_complete_route_and_reopen,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{CandidateOutcome, CandidateSource, Epoch, RouteFixture};

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
    },
];

#[test]
fn startup_activate_archived_complete_route_all_five_journal_faults_reopen_exact_durable_record() {
    for fault in JOURNAL_FAULTS {
        let fixture = RouteFixture::new(
            Epoch::Current,
            CandidateSource::Exchanged,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::Applied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let expected = fixture.expected_successor();
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        (fault.arm)();

        let error = persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority).unwrap_err();

        (fault.assert_consumed)();
        assert!(
            matches!(
                error,
                UsrRollbackActivateArchivedCompleteRoutePersistenceError::Advance { durable, .. }
                    if durable == fault.durable
            ),
            "expected {:?}, got {error:?}",
            fault.durable
        );
        match fault.durable {
            DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved => {
                assert_eq!(fixture.canonical_record(), fixture.source)
            }
            DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete => {
                assert_eq!(fixture.canonical_record(), expected)
            }
        }
        assert_eq!(fixture.database_snapshot(), database_before);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
        fixture.assert_exact_database_pair();
        fixture.assert_exact_archived_topology();
    }
}
