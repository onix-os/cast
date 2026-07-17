//! Fresh-handle restart contracts for both durable journal-fault sides.

use crate::{
    client::{
        startup_reconciliation::{
            active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
        },
        startup_recovery::DurableUsrRollbackActiveReblitCandidatePreserveRecord,
    },
    transition_journal::{
        Phase, RollbackActionOutcome, arm_next_temporary_sync_fault, arm_next_update_first_directory_sync_fault,
        assert_temporary_sync_fault_consumed, assert_update_first_directory_sync_fault_consumed,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, assert_pending_phase, assert_persistence_advance, build_active, canonical_record,
        enter_candidate, enter_fresh_handles, expected_candidate_preserved, install_persistent_database,
        release_candidate_handles,
    },
};

#[test]
fn startup_active_reblit_candidate_dispatch_source_durable_failure_fresh_entry_finishes_without_second_exchange() {
    let mut fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::Applied,
    );
    install_persistent_database(&mut fixture);
    let source = fixture.candidate_intent.clone();
    let expected = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_next_temporary_sync_fault();

    let first = enter_candidate(&fixture);

    assert_temporary_sync_fault_consumed();
    assert_persistence_advance(&first, DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);

    let retained = release_candidate_handles(fixture);
    let second = enter_fresh_handles(retained.path());

    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(canonical_record(retained.path()), expected);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
}

#[test]
fn startup_active_reblit_candidate_dispatch_successor_durable_failure_fresh_entry_never_redispatches_exchange() {
    let mut fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::Applied,
    );
    install_persistent_database(&mut fixture);
    let expected = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_next_update_first_directory_sync_fault();

    let first = enter_candidate(&fixture);

    assert_update_first_directory_sync_fault_consumed();
    assert_persistence_advance(
        &first,
        DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
    );
    assert_eq!(fixture.fixture.canonical_record(), expected);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);

    let retained = release_candidate_handles(fixture);
    let second = enter_fresh_handles(retained.path());

    assert_pending_phase(&second, Phase::CandidatePreserved);
    assert_eq!(canonical_record(retained.path()), expected);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
}
