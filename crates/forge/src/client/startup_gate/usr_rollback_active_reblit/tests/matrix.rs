//! Current and historical Apply/Finish matrices through the real startup gate.

use crate::{
    client::startup_reconciliation::{
        active_reblit_candidate_preserve_exchange_attempt_count,
        reset_active_reblit_candidate_preserve_exchange_attempt_count,
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_pending_phase, build_active, enter_candidate,
        expected_candidate_preserved,
    },
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_active_reblit_candidate_dispatch_applied_matrix_uses_one_nonzero_wrapper_exchange() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                let fixture = build_active(epoch, source, usr_outcome, CandidateOrigin::Applied);
                let expected = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
                let database_before = fixture.fixture.database_snapshot();
                reset_active_reblit_candidate_preserve_exchange_attempt_count();

                let error = enter_candidate(&fixture);

                assert_pending_phase(&error, Phase::CandidatePreserved);
                assert_eq!(fixture.fixture.canonical_record(), expected);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1);
                assert!(active_wrapper_path(&fixture).join("usr").is_dir());
            }
        }
    }
}

#[test]
fn startup_active_reblit_candidate_dispatch_finish_matrix_preserves_without_exchange() {
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                let fixture = build_active(epoch, source, usr_outcome, CandidateOrigin::AlreadySatisfied);
                let expected = expected_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
                let database_before = fixture.fixture.database_snapshot();
                reset_active_reblit_candidate_preserve_exchange_attempt_count();

                let error = enter_candidate(&fixture);

                assert_pending_phase(&error, Phase::CandidatePreserved);
                assert_eq!(fixture.fixture.canonical_record(), expected);
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
                assert!(active_wrapper_path(&fixture).join("usr").is_dir());
            }
        }
    }
}
