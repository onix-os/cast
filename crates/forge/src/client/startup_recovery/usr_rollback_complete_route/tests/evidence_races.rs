use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCompleteRouteAdmission, arm_before_usr_rollback_complete_route_fresh_namespace_capture,
            arm_between_usr_rollback_complete_route_database_captures, fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::{
            UsrRollbackCompleteRoutePersistenceError, arm_before_usr_rollback_complete_route_final_revalidation,
            persist_usr_rollback_complete_route_and_reopen,
        },
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore, encode},
};

use super::support::{
    CandidateResult, FreshDbOutcome, RouteFixture, Source, canonical_journal, create_private_directory,
    transition_quarantine_path,
};

#[test]
fn startup_usr_rollback_complete_route_rejects_reopened_and_cross_root_journals() {
    for origin in FreshDbOutcome::ALL {
        let fixture = RouteFixture::new(
            origin,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::Applied,
        );
        let first = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&first, &reservation);
        drop(first);
        let reopened = fixture.open_journal();

        let error = persist_usr_rollback_complete_route_and_reopen(reopened, authority).unwrap_err();

        assert!(matches!(error, UsrRollbackCompleteRoutePersistenceError::Authority(_)));
        assert_eq!(fixture.canonical_record(), fixture.source);
        fixture.assert_no_second_removal();
        drop(reservation);

        // Construct the foreign root first: the thread-local removal count
        // must describe the source-bound fixture built last.
        let second = RouteFixture::new(
            origin,
            Source::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateResult::AlreadySatisfied,
        );
        let first = RouteFixture::new(
            origin,
            Source::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateResult::AlreadySatisfied,
        );
        fs::write(
            canonical_journal(&second.fixture.fixture.fixture.installation.root),
            first.canonical_bytes(),
        )
        .unwrap();
        let source_journal = first.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = first.capture_ready(&source_journal, &reservation);
        let foreign = TransitionJournalStore::open_retained(
            second.fixture.fixture.fixture.installation.root_directory(),
            &second.fixture.fixture.fixture.installation.root,
        )
        .unwrap();

        let error = persist_usr_rollback_complete_route_and_reopen(foreign, authority).unwrap_err();

        assert!(matches!(error, UsrRollbackCompleteRoutePersistenceError::Authority(_)));
        assert_eq!(source_journal.load().unwrap(), Some(first.source.clone()));
        assert_eq!(first.canonical_record(), first.source);
        assert_eq!(second.canonical_record(), first.source);
        first.assert_no_second_removal();
    }
}

#[derive(Clone, Copy, Debug)]
enum FinalRace {
    Database,
    Journal,
    Installation,
    Namespace,
}

#[test]
fn startup_usr_rollback_complete_route_capture_and_final_evidence_races_never_advance() {
    for capture_namespace in [false, true] {
        let fixture = RouteFixture::new(
            FreshDbOutcome::Applied,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::Applied,
        );
        if capture_namespace {
            let target = transition_quarantine_path(&fixture.fixture.fixture.fixture, &fixture.source);
            arm_between_usr_rollback_complete_route_database_captures(move || {
                fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
            });
        } else {
            let database = fixture.fixture.fixture.fixture.database.clone();
            let previous = fixture.fixture.fixture.fixture.previous_state;
            arm_between_usr_rollback_complete_route_database_captures(move || {
                database.remove(&previous).unwrap();
            });
        }
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let admission = fixture.capture(&journal, &reservation);
        assert!(
            admission.is_err() || matches!(admission.unwrap(), UsrRollbackCompleteRouteAdmission::Deferred),
            "capture_namespace={capture_namespace}"
        );
        assert_eq!(fixture.canonical_record(), fixture.source);
        assert_eq!(fresh_db_invalidation_removal_call_count(), 1);
        fixture.fixture.assert_exact_joint_absence();
    }

    for origin in FreshDbOutcome::ALL {
        for race in [
            FinalRace::Database,
            FinalRace::Journal,
            FinalRace::Installation,
            FinalRace::Namespace,
        ] {
            let fixture = RouteFixture::new(
                origin,
                Source::Intent,
                RollbackActionOutcome::AlreadySatisfied,
                CandidateResult::AlreadySatisfied,
            );
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = fixture.capture_ready(&journal, &reservation);
            arm_final_race(&fixture, race);

            let error = persist_usr_rollback_complete_route_and_reopen(journal, authority).unwrap_err();

            assert!(
                matches!(error, UsrRollbackCompleteRoutePersistenceError::Authority(_)),
                "origin={origin:?} race={race:?}: {error:?}"
            );
            if matches!(race, FinalRace::Database | FinalRace::Namespace) {
                assert_eq!(fixture.canonical_record(), fixture.source, "{race:?}");
            }
            fixture.fixture.assert_exact_joint_absence();
            assert_eq!(
                fresh_db_invalidation_removal_call_count(),
                origin.expected_removals(),
                "{race:?}"
            );
        }
    }
}

fn arm_final_race(fixture: &RouteFixture, race: FinalRace) {
    let hook: Box<dyn FnOnce()> = match race {
        FinalRace::Database => {
            let database = fixture.fixture.fixture.fixture.database.clone();
            let previous = fixture.fixture.fixture.fixture.previous_state;
            Box::new(move || database.remove(&previous).unwrap())
        }
        FinalRace::Journal => {
            let canonical = canonical_journal(&fixture.fixture.fixture.fixture.installation.root);
            let changed = encode(&fixture.expected_successor()).unwrap();
            Box::new(move || fs::write(canonical, changed).unwrap())
        }
        FinalRace::Installation => {
            let cast = fixture.fixture.fixture.fixture.installation.root.join(".cast");
            let displaced = fixture
                .fixture
                .fixture
                .fixture
                .installation
                .root
                .join(".cast-rollback-complete-route-rebound");
            Box::new(move || {
                fs::rename(&cast, displaced).unwrap();
                fs::create_dir(&cast).unwrap();
                fs::set_permissions(cast, fs::Permissions::from_mode(0o700)).unwrap();
            })
        }
        FinalRace::Namespace => {
            let target = transition_quarantine_path(&fixture.fixture.fixture.fixture, &fixture.source);
            Box::new(move || {
                arm_before_usr_rollback_complete_route_fresh_namespace_capture(move || {
                    fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
                });
            })
        }
    };
    arm_before_usr_rollback_complete_route_final_revalidation(hook);
}

#[test]
fn startup_usr_rollback_complete_route_refuses_namespace_lookalikes() {
    for case in 0..3 {
        let fixture = RouteFixture::new(
            FreshDbOutcome::AlreadySatisfied,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::AlreadySatisfied,
        );
        let target = transition_quarantine_path(&fixture.fixture.fixture.fixture, &fixture.source);
        match case {
            0 => fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap(),
            1 => fs::rename(
                &target,
                fixture
                    .fixture
                    .fixture
                    .fixture
                    .installation
                    .state_quarantine_dir()
                    .join("displaced-rollback-complete-route-target"),
            )
            .unwrap(),
            2 => create_private_directory(
                &fixture
                    .fixture
                    .fixture
                    .fixture
                    .installation
                    .root
                    .join(".cast/root")
                    .join(fixture.fixture.fixture.fixture.candidate_state.to_string()),
            ),
            _ => unreachable!(),
        }
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(
            matches!(
                fixture.capture(&journal, &reservation).unwrap(),
                UsrRollbackCompleteRouteAdmission::Deferred
            ),
            "namespace lookalike {case} was admitted"
        );
        assert_eq!(fixture.canonical_record(), fixture.source);
        fixture.assert_no_second_removal();
    }
}
