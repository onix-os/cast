//! Ordered post-exchange durability failures for both admitted origins.

use crate::{
    client::startup_reconciliation::{
        ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint,
        active_reblit_candidate_preserve_exchange_attempt_count,
        arm_active_reblit_candidate_preserve_post_exchange_durability_fault,
        reset_active_reblit_candidate_preserve_exchange_attempt_count,
        reset_active_reblit_candidate_preserve_post_exchange_durability_events,
        take_active_reblit_candidate_preserve_post_exchange_durability_events,
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, assert_active_authority_dispatch_error, assert_pending_phase, build_active,
        enter_candidate, expected_candidate_preserved,
    },
};

const ORIGINS: [CandidateOrigin; 2] = [CandidateOrigin::Applied, CandidateOrigin::AlreadySatisfied];

const DURABILITY_FAULTS: [ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint; 6] = [
    ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::CandidateSync,
    ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::CandidateWrapperSync,
    ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::ReservationWrapperSync,
    ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::RootsParentSync,
    ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::QuarantineParentSync,
    ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::FinalPostCapture,
];

#[test]
fn startup_active_reblit_candidate_dispatch_all_six_durability_barriers_fail_at_exact_prefix_for_both_origins() {
    for origin in ORIGINS {
        for (prefix_len, fault) in DURABILITY_FAULTS.into_iter().enumerate() {
            let fixture = build_active(
                Epoch::Current,
                CandidateSource::Exchanged,
                RollbackActionOutcome::Applied,
                origin,
            );
            let source = fixture.candidate_intent.clone();
            let recovered = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
            let database_before = fixture.fixture.database_snapshot();
            reset_active_reblit_candidate_preserve_exchange_attempt_count();
            reset_active_reblit_candidate_preserve_post_exchange_durability_events();
            arm_active_reblit_candidate_preserve_post_exchange_durability_fault(fault);

            let error = enter_candidate(&fixture);

            assert_active_authority_dispatch_error(&error);
            assert_eq!(
                take_active_reblit_candidate_preserve_post_exchange_durability_events().len(),
                prefix_len,
                "{origin:?} {fault:?}"
            );
            assert_eq!(fixture.fixture.canonical_record(), source);
            assert_eq!(fixture.fixture.database_snapshot(), database_before);
            assert_eq!(
                active_reblit_candidate_preserve_exchange_attempt_count(),
                usize::from(origin == CandidateOrigin::Applied),
                "{origin:?} {fault:?}"
            );

            reset_active_reblit_candidate_preserve_post_exchange_durability_events();
            let second = enter_candidate(&fixture);

            assert_pending_phase(&second, Phase::CandidatePreserved);
            assert_eq!(
                take_active_reblit_candidate_preserve_post_exchange_durability_events().len(),
                DURABILITY_FAULTS.len(),
                "{origin:?} {fault:?} restart"
            );
            assert_eq!(fixture.fixture.canonical_record(), recovered);
            assert_eq!(fixture.fixture.database_snapshot(), database_before);
            assert_eq!(
                active_reblit_candidate_preserve_exchange_attempt_count(),
                usize::from(origin == CandidateOrigin::Applied),
                "{origin:?} {fault:?} restart"
            );
        }
    }
}
