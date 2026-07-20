//! Exact ActivateArchived CandidatePreserved completion matrix through startup.

use crate::{
    client::boot::{boot_synchronize_attempt_count, reset_boot_synchronize_attempt_count},
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::support::{
    CandidateOutcome, CandidateSource, Epoch, RouteFixture, assert_route_pending_audit, candidate_move_count,
    enter_route, reset_candidate_observers,
};

#[test]
fn startup_activate_archived_complete_route_covers_all_twenty_four_exact_candidate_preserved_cases() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let case = (epoch, source, usr_outcome, candidate_outcome);
                    let fixture = RouteFixture::new(epoch, source, usr_outcome, candidate_outcome);
                    let expected = fixture.expected_successor();
                    let canonical_before = fixture.canonical_bytes();
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();
                    reset_candidate_observers();
                    reset_retained_exchange_syscall_count();
                    reset_boot_synchronize_attempt_count();

                    let error = enter_route(&fixture);

                    assert_route_pending_audit(&error, &fixture, &expected);
                    assert_eq!(expected.phase, Phase::RollbackComplete, "{case:?}");
                    assert_eq!(expected.generation, fixture.source.generation + 1, "{case:?}");
                    assert_eq!(expected.rollback, fixture.source.rollback, "{case:?}");
                    assert_ne!(fixture.canonical_bytes(), canonical_before, "{case:?}");
                    assert_eq!(fixture.canonical_record(), expected, "{case:?}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case:?}");
                    assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case:?}");
                    fixture.assert_exact_database_pair();
                    fixture.assert_exact_archived_topology();
                    assert_eq!(candidate_move_count(), 0, "{case:?}");
                    assert_eq!(retained_exchange_syscall_count(), 0, "{case:?}");
                    assert_eq!(boot_synchronize_attempt_count(), 0, "{case:?}");
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}
