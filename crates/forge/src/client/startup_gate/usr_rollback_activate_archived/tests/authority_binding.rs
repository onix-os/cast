//! Direct negative proof for journal bindings the test seal cannot widen.

use crate::{client::active_state_snapshot::ActiveStateReservation, transition_journal::RollbackActionOutcome};

use super::support::{CandidateOutcome, CandidateSource, Epoch, RouteFixture};

#[test]
fn startup_activate_archived_complete_route_rejects_reopened_and_cross_root_journal_bindings() {
    let fixture = RouteFixture::new(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
    );
    let other = RouteFixture::new(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
    );
    let fixture_database = fixture.database_snapshot();
    let fixture_namespace = fixture.namespace_snapshot();
    let other_database = other.database_snapshot();
    let other_namespace = other.namespace_snapshot();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);

    authority.revalidate(&journal).unwrap();
    drop(journal);

    let reopened = fixture.open_journal();
    let reopened_error = authority.revalidate(&reopened).unwrap_err();
    assert_eq!(
        reopened_error.to_string(),
        "ActivateArchived rollback-completion authority was paired with a different open journal store"
    );
    drop(reopened);

    let foreign = other.open_journal();
    let foreign_error = authority.revalidate(&foreign).unwrap_err();
    assert_eq!(
        foreign_error.to_string(),
        "ActivateArchived rollback-completion authority was paired with a different open journal store"
    );

    assert_eq!(fixture.canonical_record(), fixture.source);
    assert_eq!(fixture.database_snapshot(), fixture_database);
    assert_eq!(fixture.namespace_snapshot(), fixture_namespace);
    assert_eq!(other.canonical_record(), other.source);
    assert_eq!(other.database_snapshot(), other_database);
    assert_eq!(other.namespace_snapshot(), other_namespace);
}
