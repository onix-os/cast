use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::UsrRollbackFreshDbInvalidationRouteAdmission,
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::{
    super::candidate_test_support::{CandidateLayout, CandidatePreserveFixture},
    super::test_fixture::OperationKind,
    support::{CandidateOutcome, CandidateSource, RouteFixture, capture_record},
};

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_admits_exact_current_and_historical_evidence() {
    for historical in [false, true] {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let fixture = if historical {
                        RouteFixture::historical(source, usr_outcome, candidate_outcome)
                    } else {
                        RouteFixture::new(source, usr_outcome, candidate_outcome)
                    };
                    assert!(
                        fixture
                            .fixture
                            .fixture
                            .database
                            .audit_in_flight_transition()
                            .unwrap()
                            .is_some(),
                        "{historical} {source:?} {usr_outcome:?} {candidate_outcome:?}"
                    );
                    assert!(
                        fixture
                            .fixture
                            .fixture
                            .database
                            .metadata_provenance(fixture.fixture.fixture.candidate_state)
                            .unwrap()
                            .is_some(),
                        "{historical} {source:?} {usr_outcome:?} {candidate_outcome:?}"
                    );
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    authority.revalidate(&journal).unwrap();
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_defers_inexact_phase_plan_database_and_provenance() {
    let fixture = RouteFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        capture_record(
            &fixture.fixture,
            &journal,
            &reservation,
            &fixture.fixture.candidate_intent,
        )
        .unwrap(),
        UsrRollbackFreshDbInvalidationRouteAdmission::NotApplicable
    ));
    let successor = fixture.expected_successor();
    assert_eq!(successor.phase, Phase::FreshDbInvalidationIntent);
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &successor).unwrap(),
        UsrRollbackFreshDbInvalidationRouteAdmission::NotApplicable
    ));
    drop(journal);
    drop(reservation);

    let archived = CandidatePreserveFixture::new(
        OperationKind::Archived,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateLayout::Preserved,
    );
    let archived_preserved = archived
        .candidate_intent
        .rollback_successor(Some(RollbackActionOutcome::Applied))
        .unwrap();
    assert_eq!(archived_preserved.phase, Phase::CandidatePreserved);
    let journal = archived.open_journal();
    journal
        .advance(&archived.candidate_intent, &archived_preserved)
        .unwrap();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        capture_record(&archived, &journal, &reservation, &archived_preserved).unwrap(),
        UsrRollbackFreshDbInvalidationRouteAdmission::NotApplicable
    ));
    drop(journal);
    drop(reservation);

    let cleared_row = RouteFixture::new(
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
    );
    cleared_row
        .fixture
        .fixture
        .database
        .clear_transition_if_matches(
            cleared_row.fixture.fixture.candidate_state,
            &cleared_row.source.transition_id,
        )
        .unwrap();
    let journal = cleared_row.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        cleared_row.capture(&journal, &reservation).unwrap(),
        UsrRollbackFreshDbInvalidationRouteAdmission::Deferred
    ));
    drop(journal);
    drop(reservation);

    let missing_row = RouteFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
    );
    missing_row
        .fixture
        .fixture
        .database
        .remove_transition_if_matches(
            missing_row.fixture.fixture.candidate_state,
            &missing_row.source.transition_id,
        )
        .unwrap();
    let journal = missing_row.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        missing_row.capture(&journal, &reservation).unwrap(),
        UsrRollbackFreshDbInvalidationRouteAdmission::Deferred
    ));
    drop(journal);
    drop(reservation);

    let missing_provenance = RouteFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::Applied,
    );
    missing_provenance
        .fixture
        .fixture
        .database
        .delete_metadata_provenance_for_test(missing_provenance.fixture.fixture.candidate_state)
        .unwrap();
    let journal = missing_provenance.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        missing_provenance.capture(&journal, &reservation).unwrap(),
        UsrRollbackFreshDbInvalidationRouteAdmission::Deferred
    ));
}
