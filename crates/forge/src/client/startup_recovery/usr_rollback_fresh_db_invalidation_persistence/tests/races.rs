use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::fresh_db_invalidation_removal_call_count,
        startup_recovery::{
            UsrRollbackFreshDbInvalidationPersistenceError,
            arm_before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation,
            persist_usr_rollback_fresh_db_invalidation_and_reopen,
        },
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::support::{
    CandidateResult, Fixture, FreshDbInvalidationOrigin, Source, canonical_journal, effect_authority,
    fixture_for_origin, transition_quarantine_path,
};

#[test]
fn startup_fresh_db_invalidation_persistence_rejects_reopened_and_cross_root_journals() {
    for origin in FreshDbInvalidationOrigin::ALL {
        {
            let fixture = fixture_for_origin(
                origin,
                false,
                Source::Exchanged,
                RollbackActionOutcome::Applied,
                CandidateResult::Applied,
            );
            let first = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = effect_authority(&fixture, &first, &reservation, origin);
            let expected_removals = if origin == FreshDbInvalidationOrigin::Applied {
                1
            } else {
                0
            };
            drop(first);
            let reopened = fixture.open_journal();

            let error = persist_usr_rollback_fresh_db_invalidation_and_reopen(reopened, authority).unwrap_err();

            assert!(matches!(
                error,
                UsrRollbackFreshDbInvalidationPersistenceError::Authority(_)
            ));
            assert_eq!(fixture.canonical_record(), fixture.record);
            assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
        }

        {
            let first_fixture = fixture_for_origin(
                origin,
                false,
                Source::Intent,
                RollbackActionOutcome::AlreadySatisfied,
                CandidateResult::AlreadySatisfied,
            );
            let second_fixture = fixture_for_origin(
                origin,
                false,
                Source::Intent,
                RollbackActionOutcome::AlreadySatisfied,
                CandidateResult::AlreadySatisfied,
            );
            let first = first_fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = effect_authority(&first_fixture, &first, &reservation, origin);
            let expected_removals = if origin == FreshDbInvalidationOrigin::Applied {
                1
            } else {
                0
            };
            drop(first);
            fs::write(
                canonical_journal(&second_fixture.fixture.fixture.installation.root),
                first_fixture.canonical_bytes(),
            )
            .unwrap();
            let foreign = TransitionJournalStore::open_retained(
                second_fixture.fixture.fixture.installation.root_directory(),
                &second_fixture.fixture.fixture.installation.root,
            )
            .unwrap();

            let error = persist_usr_rollback_fresh_db_invalidation_and_reopen(foreign, authority).unwrap_err();

            assert!(matches!(
                error,
                UsrRollbackFreshDbInvalidationPersistenceError::Authority(_)
            ));
            assert_eq!(first_fixture.canonical_record(), first_fixture.record);
            assert_eq!(second_fixture.canonical_record(), first_fixture.record);
            assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
        }
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
fn startup_fresh_db_invalidation_persistence_final_races_fail_before_advance() {
    for origin in FreshDbInvalidationOrigin::ALL {
        for race in [
            FinalRace::Database,
            FinalRace::Journal,
            FinalRace::Installation,
            FinalRace::Namespace,
        ] {
            let fixture = fixture_for_origin(
                origin,
                false,
                Source::Exchanged,
                RollbackActionOutcome::Applied,
                CandidateResult::AlreadySatisfied,
            );
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = effect_authority(&fixture, &journal, &reservation, origin);
            let expected_removals = if origin == FreshDbInvalidationOrigin::Applied {
                1
            } else {
                0
            };
            arm_final_race(&fixture, race);

            let error = persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap_err();

            assert!(
                matches!(error, UsrRollbackFreshDbInvalidationPersistenceError::Authority(_)),
                "origin={origin:?} race={race:?}: {error:?}"
            );
            if matches!(race, FinalRace::Database | FinalRace::Namespace) {
                assert_eq!(fixture.canonical_record(), fixture.record, "{race:?}");
            }
            assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
        }
    }
}

fn arm_final_race(fixture: &Fixture, race: FinalRace) {
    let hook: Box<dyn FnOnce()> = match race {
        FinalRace::Database => {
            let database = fixture.fixture.fixture.database.clone();
            let previous = fixture.fixture.fixture.previous_state;
            Box::new(move || database.remove(&previous).unwrap())
        }
        FinalRace::Journal => Box::new(fixture.journal_change_hook()),
        FinalRace::Installation => {
            let cast = fixture.fixture.fixture.installation.root.join(".cast");
            let displaced = fixture
                .fixture
                .fixture
                .installation
                .root
                .join(".cast-fresh-db-persistence-rebound");
            Box::new(move || {
                fs::rename(&cast, displaced).unwrap();
                fs::create_dir(&cast).unwrap();
                fs::set_permissions(cast, fs::Permissions::from_mode(0o700)).unwrap();
            })
        }
        FinalRace::Namespace => {
            let target = transition_quarantine_path(&fixture.fixture.fixture, &fixture.record);
            Box::new(move || fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap())
        }
    };
    arm_before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation(hook);
}
