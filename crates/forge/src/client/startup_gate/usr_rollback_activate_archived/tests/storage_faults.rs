//! All five conditional journal-update faults through production startup.

use crate::{
    client::startup_recovery::DurableUsrRollbackActivateArchivedCompleteRouteRecord,
    transition_journal::{
        RollbackActionOutcome, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::support::{
    CandidateOutcome, CandidateSource, Epoch, RouteFixture, assert_complete_persistence_advance, assert_pending_phase,
    candidate_move_count, canonical_record_from_root, enter_candidate_with_fresh_handles, enter_route,
    install_persistent_route_database, release_route_handles, reset_candidate_observers,
};

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
        let mut fixture = RouteFixture::new(
            Epoch::Current,
            CandidateSource::Exchanged,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::Applied,
        );
        install_persistent_route_database(&mut fixture);
        let expected = fixture.expected_successor();
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        reset_candidate_observers();
        (fault.arm)();

        let error = enter_route(&fixture);

        (fault.assert_consumed)();
        assert_complete_persistence_advance(&error, fault.durable);
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
        assert_eq!(candidate_move_count(), 0);

        let retained = release_route_handles(fixture);
        let second = enter_candidate_with_fresh_handles(retained.path());

        assert_pending_phase(&second, crate::transition_journal::Phase::RollbackComplete);
        assert_eq!(canonical_record_from_root(retained.path()), expected);
        assert_eq!(candidate_move_count(), 0);
    }
}
