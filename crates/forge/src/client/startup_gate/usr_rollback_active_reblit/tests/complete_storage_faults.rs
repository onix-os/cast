//! All five conditional journal-update faults at the completion route.

use crate::{
    client::startup_recovery::DurableUsrRollbackActiveReblitCompleteRouteRecord,
    transition_journal::{
        RollbackActionOutcome, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_complete_persistence_advance,
        assert_complete_route_journal_only, assert_exact_no_boot_completion_plan, build_active, enter_candidate,
        expected_rollback_complete, persist_candidate_preserved, reset_complete_route_effect_observers,
    },
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableUsrRollbackActiveReblitCompleteRouteRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
    },
];

#[test]
fn startup_active_reblit_complete_route_all_five_journal_faults_reopen_exact_durable_record() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for candidate_source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOrigin::ALL {
                    for fault in JOURNAL_FAULTS {
                        let case = (epoch, candidate_source, usr_outcome, candidate_outcome);
                        let fixture = build_active(
                            epoch,
                            candidate_source,
                            usr_outcome,
                            CandidateOrigin::AlreadySatisfied,
                        );
                        let source = persist_candidate_preserved(&fixture, candidate_outcome);
                        let expected = expected_rollback_complete(&source);
                        let database_before = fixture.fixture.database_snapshot();
                        let namespace_before = fixture.fixture.namespace_snapshot();
                        assert_exact_no_boot_completion_plan(&source, candidate_source);
                        reset_complete_route_effect_observers();
                        (fault.arm)();

                        let error = enter_candidate(&fixture);

                        (fault.assert_consumed)();
                        assert_complete_persistence_advance(&error, fault.durable);
                        match fault.durable {
                            DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved => {
                                assert_eq!(fixture.fixture.canonical_record(), source, "{case:?}")
                            }
                            DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete => {
                                assert_eq!(fixture.fixture.canonical_record(), expected, "{case:?}")
                            }
                        }
                        assert_eq!(fixture.fixture.database_snapshot(), database_before, "{case:?}");
                        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before, "{case:?}");
                        assert!(active_wrapper_path(&fixture).join("usr").is_dir(), "{case:?}");
                        assert_complete_route_journal_only();
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, 120);
}
