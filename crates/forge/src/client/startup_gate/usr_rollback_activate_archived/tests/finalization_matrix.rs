//! Exact terminal ActivateArchived matrix through the production startup gate.

use crate::transition_journal::{Operation, RollbackActionOutcome};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, RouteFixture, assert_canonical_absent, candidate_move_count, enter_clean_route,
        persist_rollback_complete, reset_candidate_observers,
    },
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

#[test]
fn startup_activate_archived_finalization_covers_all_sixteen_exact_terminal_cases() {
    assert_eq!(Epoch::ALL.len(), 2);
    assert_eq!(CandidateSource::ALL.len(), 2);
    assert_eq!(USR_OUTCOMES.len(), 2);
    assert_eq!(CandidateOutcome::ALL.len(), 2);
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOutcome::ALL {
                    let fixture = RouteFixture::new(epoch, source, usr_outcome, candidate_outcome);
                    let terminal = persist_rollback_complete(&fixture);
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();
                    reset_candidate_observers();

                    let clean = enter_clean_route(&fixture);

                    assert_canonical_absent(&fixture.fixture.fixture.installation.root);
                    assert_eq!(terminal.operation, Operation::ActivateArchived);
                    assert_eq!(fixture.database_snapshot(), database_before);
                    assert_eq!(fixture.namespace_snapshot(), namespace_before);
                    fixture.assert_exact_archived_topology();
                    assert_eq!(candidate_move_count(), 0);
                    drop(clean);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 16);
}
