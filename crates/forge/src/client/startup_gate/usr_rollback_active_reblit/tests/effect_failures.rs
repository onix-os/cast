//! Raw one-shot exchange reports classified only from fresh evidence.

use crate::{
    client::startup_reconciliation::{
        ActiveReblitCandidatePreserveExchangeFault, active_reblit_candidate_preserve_exchange_attempt_count,
        arm_active_reblit_candidate_preserve_exchange_fault,
        reset_active_reblit_candidate_preserve_exchange_attempt_count,
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, assert_not_applied, assert_pending_phase, build_active, enter_candidate,
        expected_candidate_preserved,
    },
};

#[test]
fn startup_active_reblit_candidate_dispatch_classifies_all_three_raw_exchange_reports_from_evidence() {
    for fault in [
        ActiveReblitCandidatePreserveExchangeFault::ErrorWithoutApply,
        ActiveReblitCandidatePreserveExchangeFault::SuccessWithoutApply,
    ] {
        let fixture = build_active(
            Epoch::Current,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOrigin::Applied,
        );
        let source = fixture.candidate_intent.clone();
        let expected = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        reset_active_reblit_candidate_preserve_exchange_attempt_count();
        arm_active_reblit_candidate_preserve_exchange_fault(fault);

        let error = enter_candidate(&fixture);

        assert_not_applied(error);
        assert_eq!(fixture.fixture.canonical_record(), source);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);

        let second = enter_candidate(&fixture);

        assert_pending_phase(&second, Phase::CandidatePreserved);
        assert_eq!(fixture.fixture.canonical_record(), expected);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 2);
    }

    let fixture = build_active(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::Applied,
    );
    let expected = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let database_before = fixture.fixture.database_snapshot();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_active_reblit_candidate_preserve_exchange_fault(ActiveReblitCandidatePreserveExchangeFault::ErrorAfterApply);

    let error = enter_candidate(&fixture);

    assert_pending_phase(&error, Phase::CandidatePreserved);
    assert_eq!(fixture.fixture.canonical_record(), expected);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
}
