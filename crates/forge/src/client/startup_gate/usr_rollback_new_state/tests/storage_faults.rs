use crate::transition_journal::{
    Phase, RollbackActionOutcome, arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
    arm_next_update_exchange_fault, arm_next_update_final_directory_sync_fault,
    arm_next_update_first_directory_sync_fault, assert_displaced_unlink_fault_consumed,
    assert_temporary_sync_fault_consumed, assert_update_exchange_fault_consumed,
    assert_update_final_directory_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, FreshOutcome, TargetPrefix, assert_pending_phase, assert_suffix_dispatch_error,
        build_candidate, build_fresh_invalidation, effect_counts, enter_candidate, enter_invalidation,
        persist_candidate_preserved, persist_fresh_invalidated, reset_namespace_effect_counts,
    },
};

#[derive(Clone, Copy)]
struct JournalFault {
    arm: fn(),
    assert_consumed: fn(),
    successor_durable: bool,
}

const JOURNAL_FAULTS: [JournalFault; 5] = [
    JournalFault {
        arm: arm_next_temporary_sync_fault,
        assert_consumed: assert_temporary_sync_fault_consumed,
        successor_durable: false,
    },
    JournalFault {
        arm: arm_next_update_exchange_fault,
        assert_consumed: assert_update_exchange_fault_consumed,
        successor_durable: false,
    },
    JournalFault {
        arm: arm_next_update_first_directory_sync_fault,
        assert_consumed: assert_update_first_directory_sync_fault_consumed,
        successor_durable: true,
    },
    JournalFault {
        arm: arm_next_displaced_unlink_fault,
        assert_consumed: assert_displaced_unlink_fault_consumed,
        successor_durable: true,
    },
    JournalFault {
        arm: arm_next_update_final_directory_sync_fault,
        assert_consumed: assert_update_final_directory_sync_fault_consumed,
        successor_durable: true,
    },
];

#[test]
fn startup_new_state_suffix_all_five_journal_faults_reenter_each_of_four_persistence_boundaries_exactly() {
    for fault in JOURNAL_FAULTS {
        exercise_candidate_persistence(fault);
        exercise_candidate_preserved_route(fault);
        exercise_fresh_database_persistence(fault);
        exercise_rollback_complete_route(fault);
    }
}

fn exercise_candidate_persistence(fault: JournalFault) {
    let fixture = build_candidate(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        TargetPrefix::Canonical,
    );
    let source = fixture.candidate_intent.clone();
    let applied = source.rollback_successor(Some(RollbackActionOutcome::Applied)).unwrap();
    reset_namespace_effect_counts();
    (fault.arm)();

    let first = enter_candidate(&fixture);

    (fault.assert_consumed)();
    assert_suffix_dispatch_error(&first);
    assert_eq!(
        fixture.fixture.canonical_record(),
        if fault.successor_durable {
            applied.clone()
        } else {
            source.clone()
        }
    );
    assert_eq!(effect_counts().candidate_move, 1);

    let second = enter_candidate(&fixture);
    if fault.successor_durable {
        assert_pending_phase(&second, Phase::FreshDbInvalidationIntent);
        assert_eq!(
            fixture.fixture.canonical_record(),
            applied.rollback_successor(None).unwrap()
        );
    } else {
        assert_pending_phase(&second, Phase::CandidatePreserved);
        assert_eq!(
            fixture.fixture.canonical_record(),
            source
                .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
                .unwrap()
        );
    }
    assert_eq!(effect_counts().candidate_move, 1);
}

fn exercise_candidate_preserved_route(fault: JournalFault) {
    let fixture = build_candidate(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        TargetPrefix::Preserved,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOutcome::AlreadySatisfied);
    let intent = source.rollback_successor(None).unwrap();
    reset_namespace_effect_counts();
    let removal_before = effect_counts().fresh_removal;
    (fault.arm)();

    let first = enter_candidate(&fixture);

    (fault.assert_consumed)();
    assert_suffix_dispatch_error(&first);
    assert_eq!(
        fixture.fixture.canonical_record(),
        if fault.successor_durable {
            intent.clone()
        } else {
            source.clone()
        }
    );
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, removal_before);

    let second = enter_candidate(&fixture);
    if fault.successor_durable {
        assert_pending_phase(&second, Phase::FreshDbInvalidated);
        assert_eq!(effect_counts().fresh_removal, 1);
    } else {
        assert_pending_phase(&second, Phase::FreshDbInvalidationIntent);
        assert_eq!(fixture.fixture.canonical_record(), intent);
        assert_eq!(effect_counts().fresh_removal, removal_before);
    }
    assert_eq!(effect_counts().candidate_move, 0);
}

fn exercise_fresh_database_persistence(fault: JournalFault) {
    let fixture = build_fresh_invalidation(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
        FreshOutcome::Applied,
    );
    let source = fixture.record.clone();
    let applied = source.rollback_successor(Some(RollbackActionOutcome::Applied)).unwrap();
    (fault.arm)();

    let first = enter_invalidation(&fixture);

    (fault.assert_consumed)();
    assert_suffix_dispatch_error(&first);
    assert_eq!(
        fixture.canonical_record(),
        if fault.successor_durable {
            applied.clone()
        } else {
            source.clone()
        }
    );
    fixture.assert_exact_joint_absence();
    assert_eq!(effect_counts().fresh_removal, 1);

    let second = enter_invalidation(&fixture);
    if fault.successor_durable {
        assert_pending_phase(&second, Phase::RollbackComplete);
        assert_eq!(fixture.canonical_record(), applied.rollback_successor(None).unwrap());
        assert_eq!(effect_counts().fresh_removal, 1);
    } else {
        assert_pending_phase(&second, Phase::FreshDbInvalidated);
        assert_eq!(
            fixture.canonical_record(),
            source
                .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
                .unwrap()
        );
        assert_eq!(effect_counts().fresh_removal, 0);
    }
}

fn exercise_rollback_complete_route(fault: JournalFault) {
    let fixture = build_fresh_invalidation(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
        FreshOutcome::AlreadySatisfied,
    );
    let source = persist_fresh_invalidated(&fixture, FreshOutcome::AlreadySatisfied);
    let complete = source.rollback_successor(None).unwrap();
    let removal_before = effect_counts().fresh_removal;
    (fault.arm)();

    let first = enter_invalidation(&fixture);

    (fault.assert_consumed)();
    assert_suffix_dispatch_error(&first);
    assert_eq!(
        fixture.canonical_record(),
        if fault.successor_durable {
            complete.clone()
        } else {
            source.clone()
        }
    );
    assert_eq!(effect_counts().fresh_removal, removal_before);

    let second = enter_invalidation(&fixture);
    assert_pending_phase(&second, Phase::RollbackComplete);
    assert_eq!(fixture.canonical_record(), complete);
    assert_eq!(effect_counts().fresh_removal, removal_before);
}
