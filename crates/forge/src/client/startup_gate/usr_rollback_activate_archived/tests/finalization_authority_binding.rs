//! Direct negative proof for authority/store pairings startup cannot construct.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActivateArchivedFinalizationSeal,
        startup_reconciliation::{
            UsrRollbackActivateArchivedFinalizationAdmission, UsrRollbackActivateArchivedFinalizationAuthority,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, RouteFixture, candidate_move_count, persist_rollback_complete,
        reset_candidate_observers,
    },
};

#[test]
fn startup_activate_archived_finalization_authority_covers_both_epochs_and_rejects_wrong_bindings() {
    for epoch in Epoch::ALL {
        let fixture = RouteFixture::new(
            epoch,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOutcome::Applied,
        );
        let terminal = persist_rollback_complete(&fixture);
        let other = RouteFixture::new(
            Epoch::Historical,
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::AlreadySatisfied,
        );
        let _other_terminal = persist_rollback_complete(&other);
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        let other_database = other.database_snapshot();
        let other_namespace = other.namespace_snapshot();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_candidate_observers();

        fixture.assert_exact_database_pair();
        fixture.assert_exact_archived_topology();
        let authority = super::support::capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
        authority.revalidate(&journal).unwrap();
        drop(journal);

        let reopened = fixture.open_journal();
        let reopened_error = authority.revalidate(&reopened).unwrap_err();
        assert_eq!(
            reopened_error.to_string(),
            "ActivateArchived rollback-finalization authority was paired with a different open journal store"
        );
        drop(reopened);

        let other_journal = other.open_journal();
        let cross_root_error = authority.revalidate(&other_journal).unwrap_err();
        assert_eq!(
            cross_root_error.to_string(),
            "ActivateArchived rollback-finalization authority was paired with a different open journal store"
        );
        assert_eq!(fixture.canonical_record(), terminal);
        assert_eq!(fixture.database_snapshot(), database_before);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
        assert_eq!(other.database_snapshot(), other_database);
        assert_eq!(other.namespace_snapshot(), other_namespace);
        assert_eq!(candidate_move_count(), 0);
    }
}

#[test]
fn startup_activate_archived_finalization_authority_refuses_inexact_terminal_identities() {
    for mutation in 0..3 {
        let fixture = RouteFixture::new(
            Epoch::Current,
            CandidateSource::Intent,
            RollbackActionOutcome::Applied,
            CandidateOutcome::AlreadySatisfied,
        );
        let terminal = persist_rollback_complete(&fixture);
        let mut wrong = terminal.clone();
        match mutation {
            0 => wrong.candidate.id = None,
            1 => wrong.previous.id = None,
            2 => wrong.previous.id = wrong.candidate.id,
            _ => unreachable!(),
        }
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let seal = UsrRollbackActivateArchivedFinalizationSeal::new_for_test();
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        reset_candidate_observers();

        let admission = UsrRollbackActivateArchivedFinalizationAuthority::capture(
            &seal,
            &fixture.fixture.fixture.installation,
            &journal,
            &fixture.fixture.fixture.database,
            &reservation,
            &wrong,
        )
        .unwrap();

        assert!(matches!(
            admission,
            UsrRollbackActivateArchivedFinalizationAdmission::Deferred
        ));
        assert_eq!(fixture.canonical_record(), terminal);
        assert_eq!(fixture.database_snapshot(), database_before);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
        assert_eq!(candidate_move_count(), 0);
    }
}
