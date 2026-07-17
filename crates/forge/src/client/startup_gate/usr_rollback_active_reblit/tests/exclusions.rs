//! Exact phase and operation exclusions with zero Active exchange authority.

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
    support::{
        CandidateOrigin, Epoch, assert_pending_phase, build_active, build_other, enter_candidate,
        persist_candidate_preserved,
    },
};

#[test]
fn startup_active_reblit_candidate_dispatch_excludes_candidate_preserved_and_activate_archived_with_zero_effects() {
    let active = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let preserved = persist_candidate_preserved(&active, CandidateOrigin::AlreadySatisfied);
    let database_before = active.fixture.database_snapshot();
    let namespace_before = active.fixture.namespace_snapshot();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();

    let wrong_phase = enter_candidate(&active);

    assert_pending_phase(&wrong_phase, Phase::CandidatePreserved);
    assert_eq!(active.fixture.canonical_record(), preserved);
    assert_eq!(active.fixture.database_snapshot(), database_before);
    assert_eq!(active.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);

    let archived = build_other(
        OperationKind::Archived,
        CandidateSource::Exchanged,
        CandidateLayout::Staged,
    );
    let source = archived.candidate_intent.clone();
    let database_before = archived.fixture.database_snapshot();
    let namespace_before = archived.fixture.namespace_snapshot();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();

    let wrong_operation = enter_candidate(&archived);

    assert_pending_phase(&wrong_operation, Phase::CandidatePreserveIntent);
    assert_eq!(archived.fixture.canonical_record(), source);
    assert_eq!(archived.fixture.database_snapshot(), database_before);
    assert_eq!(archived.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
}
