//! All five conditional journal-update faults at the completion route.

use crate::{
    client::startup_recovery::DurableUsrRollbackActiveReblitCompleteRouteRecord,
    transition_journal::{
        Phase, RollbackActionOutcome, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_complete_persistence_advance, assert_no_candidate_effects,
        assert_pending_phase, build_active, enter_candidate, expected_rollback_complete, persist_candidate_preserved,
        reset_candidate_effect_observers,
    },
};

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
fn startup_active_reblit_complete_route_all_five_journal_faults_converge_on_second_entry() {
    for fault in JOURNAL_FAULTS {
        let fixture = build_active(
            Epoch::Current,
            CandidateSource::Exchanged,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOrigin::AlreadySatisfied,
        );
        let source = persist_candidate_preserved(&fixture, CandidateOrigin::Applied);
        let expected = expected_rollback_complete(&source);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        reset_candidate_effect_observers();
        (fault.arm)();

        let first = enter_candidate(&fixture);

        (fault.assert_consumed)();
        assert_complete_persistence_advance(&first, fault.durable);
        assert_eq!(
            fixture.fixture.canonical_record(),
            match fault.durable {
                DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved => source,
                DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete => expected.clone(),
            }
        );
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert!(active_wrapper_path(&fixture).join("usr").is_dir());
        assert_no_candidate_effects();

        let second = enter_candidate(&fixture);

        assert_pending_phase(&second, Phase::RollbackComplete);
        assert_eq!(fixture.fixture.canonical_record(), expected);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert!(active_wrapper_path(&fixture).join("usr").is_dir());
        assert_no_candidate_effects();
    }
}
