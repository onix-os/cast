//! Exact phase and operation exclusions with zero Active exchange authority.

use crate::{
    client::startup_reconciliation::{
        active_reblit_candidate_preserve_exchange_attempt_count,
        reset_active_reblit_candidate_preserve_exchange_attempt_count,
    },
    transition_journal::Phase,
};

use super::{
    super::{
        candidate_test_support::{CandidateLayout, CandidateSource},
        test_fixture::OperationKind,
    },
    support::{assert_pending_phase, build_other, enter_candidate},
};

#[test]
fn startup_active_reblit_candidate_dispatch_excludes_activate_archived_with_zero_effects() {
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
