//! NewState remains reachable after the earlier ActiveReblit discriminator.

use crate::{
    client::startup_reconciliation::{
        active_reblit_candidate_preserve_exchange_attempt_count,
        reset_active_reblit_candidate_preserve_exchange_attempt_count,
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::{
    super::{
        candidate_test_support::{CandidateLayout, CandidateSource},
        test_fixture::OperationKind,
    },
    support::{assert_pending_phase, build_other, enter_candidate},
};

#[test]
fn startup_active_reblit_candidate_dispatch_precedes_new_state_without_stealing_its_checkpoint() {
    let fixture = build_other(
        OperationKind::NewState,
        CandidateSource::Exchanged,
        CandidateLayout::Preserved,
    );
    let expected = fixture
        .candidate_intent
        .rollback_successor(Some(RollbackActionOutcome::AlreadySatisfied))
        .unwrap();
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();

    let error = enter_candidate(&fixture);

    assert_pending_phase(&error, Phase::CandidatePreserved);
    assert_eq!(fixture.fixture.canonical_record(), expected);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
}
