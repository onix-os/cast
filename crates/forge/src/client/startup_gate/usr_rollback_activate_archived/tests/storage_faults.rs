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
    CandidateOutcome, CandidateSource, Epoch, RouteFixture, assert_complete_persistence_advance,
    candidate_move_count, enter_route, reset_candidate_observers,
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
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    for fault in JOURNAL_FAULTS {
                        let case = (epoch, source, usr_outcome, candidate_outcome);
                        let fixture = RouteFixture::new(epoch, source, usr_outcome, candidate_outcome);
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
                                assert_eq!(fixture.canonical_record(), fixture.source, "{case:?}")
                            }
                            DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete => {
                                assert_eq!(fixture.canonical_record(), expected, "{case:?}")
                            }
                        }
                        assert_eq!(fixture.database_snapshot(), database_before, "{case:?}");
                        assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case:?}");
                        fixture.assert_exact_database_pair();
                        fixture.assert_exact_archived_topology();
                        assert_eq!(candidate_move_count(), 0, "{case:?}");
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, 120);
}
