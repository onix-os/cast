use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationRouteAdmission,
            arm_before_usr_rollback_fresh_db_invalidation_route_fresh_namespace_capture,
            arm_between_usr_rollback_fresh_db_invalidation_route_database_captures,
        },
        startup_recovery::{
            UsrRollbackFreshDbInvalidationRoutePersistenceError,
            arm_before_usr_rollback_fresh_db_invalidation_route_final_revalidation,
            persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
        },
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore, encode},
};

use super::support::{
    CandidateOutcome, CandidateSource, RouteFixture, canonical_journal, create_private_directory,
    transition_quarantine_path,
};

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_rejects_mixed_and_cross_root_journals() {
    for candidate_outcome in CandidateOutcome::ALL {
        let fixture = RouteFixture::new(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            candidate_outcome,
        );
        let first = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&first, &reservation);
        drop(first);
        let independently_reopened = fixture.open_journal();

        let error =
            persist_usr_rollback_fresh_db_invalidation_route_and_reopen(independently_reopened, authority).unwrap_err();

        assert!(matches!(
            error,
            UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(_)
        ));
        assert_eq!(fixture.canonical_record(), fixture.source);
        drop(reservation);

        let first = RouteFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            candidate_outcome,
        );
        let second = RouteFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            candidate_outcome,
        );
        fs::write(
            canonical_journal(&second.fixture.fixture.installation.root),
            first.canonical_bytes(),
        )
        .unwrap();
        let first_journal = first.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = first.capture_ready(&first_journal, &reservation);
        let foreign = TransitionJournalStore::open_retained(
            second.fixture.fixture.installation.root_directory(),
            &second.fixture.fixture.installation.root,
        )
        .unwrap();

        let error = persist_usr_rollback_fresh_db_invalidation_route_and_reopen(foreign, authority).unwrap_err();

        assert!(matches!(
            error,
            UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(_)
        ));
        assert_eq!(first_journal.load().unwrap(), Some(first.source.clone()));
        assert_eq!(first.canonical_record(), first.source);
        assert_eq!(second.canonical_record(), first.source);
    }
}

#[derive(Clone, Copy, Debug)]
enum FinalRace {
    Database,
    Provenance,
    Journal,
    Installation,
    Namespace,
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_capture_and_final_evidence_races_never_advance() {
    let fixture = RouteFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
    );
    let database = fixture.fixture.fixture.database.clone();
    let candidate = fixture.fixture.fixture.candidate_state;
    let transition = fixture.source.transition_id.clone();
    arm_between_usr_rollback_fresh_db_invalidation_route_database_captures(move || {
        database.clear_transition_if_matches(candidate, &transition).unwrap();
    });
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        fixture.capture(&journal, &reservation).unwrap(),
        UsrRollbackFreshDbInvalidationRouteAdmission::Deferred
    ));
    assert_eq!(fixture.canonical_record(), fixture.source);
    drop(journal);
    drop(reservation);

    for race in [
        FinalRace::Database,
        FinalRace::Provenance,
        FinalRace::Journal,
        FinalRace::Installation,
        FinalRace::Namespace,
    ] {
        let fixture = RouteFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::AlreadySatisfied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        arm_final_race(&fixture, race);

        let error = persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap_err();

        assert!(
            matches!(error, UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(_)),
            "{race:?}: {error:?}"
        );
    }
}

fn arm_final_race(fixture: &RouteFixture, race: FinalRace) {
    let hook: Box<dyn FnOnce()> = match race {
        FinalRace::Database => {
            let database = fixture.fixture.fixture.database.clone();
            let candidate = fixture.fixture.fixture.candidate_state;
            let transition = fixture.source.transition_id.clone();
            Box::new(move || {
                database.clear_transition_if_matches(candidate, &transition).unwrap();
            })
        }
        FinalRace::Provenance => {
            let database = fixture.fixture.fixture.database.clone();
            let candidate = fixture.fixture.fixture.candidate_state;
            Box::new(move || {
                database.delete_metadata_provenance_for_test(candidate).unwrap();
            })
        }
        FinalRace::Journal => {
            let canonical = canonical_journal(&fixture.fixture.fixture.installation.root);
            let changed = encode(&fixture.expected_successor()).unwrap();
            Box::new(move || fs::write(canonical, changed).unwrap())
        }
        FinalRace::Installation => {
            let cast = fixture.fixture.fixture.installation.root.join(".cast");
            let displaced = fixture
                .fixture
                .fixture
                .installation
                .root
                .join(".cast-fresh-db-route-rebound");
            Box::new(move || {
                fs::rename(&cast, displaced).unwrap();
                fs::create_dir(&cast).unwrap();
                fs::set_permissions(cast, fs::Permissions::from_mode(0o700)).unwrap();
            })
        }
        FinalRace::Namespace => {
            let target = transition_quarantine_path(&fixture.fixture.fixture, &fixture.source);
            Box::new(move || {
                arm_before_usr_rollback_fresh_db_invalidation_route_fresh_namespace_capture(move || {
                    fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
                });
            })
        }
    };
    arm_before_usr_rollback_fresh_db_invalidation_route_final_revalidation(hook);
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_refuses_namespace_lookalikes() {
    for case in 0..3 {
        let fixture = RouteFixture::new(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOutcome::AlreadySatisfied,
        );
        let target = transition_quarantine_path(&fixture.fixture.fixture, &fixture.source);
        match case {
            0 => fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap(),
            1 => fs::rename(
                &target,
                fixture
                    .fixture
                    .fixture
                    .installation
                    .state_quarantine_dir()
                    .join("displaced-fresh-db-route-target"),
            )
            .unwrap(),
            2 => create_private_directory(
                &fixture
                    .fixture
                    .fixture
                    .installation
                    .root
                    .join(".cast/root")
                    .join(fixture.fixture.fixture.candidate_state.to_string()),
            ),
            _ => unreachable!(),
        }
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(
            matches!(
                fixture.capture(&journal, &reservation).unwrap(),
                UsrRollbackFreshDbInvalidationRouteAdmission::Deferred
            ),
            "namespace lookalike {case} was admitted"
        );
        assert_eq!(fixture.canonical_record(), fixture.source);
    }
}
