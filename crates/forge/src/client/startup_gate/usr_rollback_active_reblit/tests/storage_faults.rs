//! All five conditional journal-update fault positions through startup entry.

use crate::{
    client::{
        startup_reconciliation::{
            active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
        },
        startup_recovery::DurableUsrRollbackActiveReblitCandidatePreserveRecord,
    },
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
        CandidateOrigin, Epoch, assert_pending_phase, assert_persistence_advance, build_active, enter_candidate,
        expected_candidate_preserved,
    },
};

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
    },
];

#[test]
fn startup_active_reblit_candidate_dispatch_all_five_journal_faults_reopen_exact_source_or_successor() {
    for fault in JOURNAL_FAULTS {
        let fixture = build_active(
            Epoch::Current,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOrigin::Applied,
        );
        let source = fixture.candidate_intent.clone();
        let applied = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
        let already_satisfied = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
        let database_before = fixture.fixture.database_snapshot();
        reset_active_reblit_candidate_preserve_exchange_attempt_count();
        (fault.arm)();

        let first = enter_candidate(&fixture);

        (fault.assert_consumed)();
        assert_persistence_advance(&first, fault.durable);
        assert_eq!(
            fixture.fixture.canonical_record(),
            match fault.durable {
                DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source => source,
                DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved => {
                    applied.clone()
                }
            }
        );
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);

        let second = enter_candidate(&fixture);

        assert_pending_phase(&second, Phase::CandidatePreserved);
        assert_eq!(
            fixture.fixture.canonical_record(),
            match fault.durable {
                DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source => already_satisfied,
                DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved => applied,
            }
        );
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
    }
}
