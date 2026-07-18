//! Database, provenance, journal, and namespace race boundaries.

use std::fs;

use crate::{
    client::{
        startup_gate,
        startup_reconciliation::{
            RecoveryBlocker, arm_before_usr_rollback_activate_archived_complete_route_fresh_namespace_capture,
            arm_between_usr_rollback_activate_archived_complete_route_database_captures,
        },
        startup_recovery::arm_before_usr_rollback_activate_archived_complete_route_final_revalidation,
    },
    transition_journal::{Phase, RollbackActionOutcome, encode},
};

use super::support::{
    CandidateOutcome, CandidateSource, Epoch, RouteFixture, assert_complete_persistence_authority_error,
    candidate_move_count, enter_route, reset_candidate_observers,
};

#[derive(Clone, Copy, Debug)]
enum CaptureRace {
    Database,
    Provenance,
    Namespace,
}

#[test]
fn startup_activate_archived_complete_route_capture_sandwich_rejects_database_provenance_and_namespace_races() {
    for race in [CaptureRace::Database, CaptureRace::Provenance, CaptureRace::Namespace] {
        let fixture = exact_fixture();
        let canonical_before = fixture.canonical_bytes();
        let rows_before = fixture.fixture.fixture.database.all().unwrap();
        let namespace_before = fixture.namespace_snapshot();
        let inserted = fixture
            .fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("activate-archived-capture-race");
        let hook: Box<dyn FnOnce()> = match race {
            CaptureRace::Database => {
                let database = fixture.fixture.fixture.database.clone();
                let candidate = fixture.fixture.fixture.candidate_state;
                Box::new(move || database.remove(&candidate).unwrap())
            }
            CaptureRace::Provenance => {
                let database = fixture.fixture.fixture.database.clone();
                let candidate = fixture.fixture.fixture.candidate_state;
                Box::new(move || database.delete_metadata_provenance_for_test(candidate).unwrap())
            }
            CaptureRace::Namespace => Box::new(
                fixture
                    .fixture
                    .namespace_change_hook("activate-archived-capture-race".to_owned()),
            ),
        };
        arm_between_usr_rollback_activate_archived_complete_route_database_captures(hook);
        reset_candidate_observers();

        let error = enter_route(&fixture);

        assert_capture_race_pending(&error, race);
        assert_eq!(fixture.canonical_bytes(), canonical_before, "{race:?}");
        assert_eq!(fixture.canonical_record(), fixture.source, "{race:?}");
        assert_eq!(candidate_move_count(), 0, "{race:?}");
        match race {
            CaptureRace::Database => {
                assert_eq!(
                    fixture.fixture.fixture.database.all().unwrap().len(),
                    rows_before.len() - 1
                );
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            CaptureRace::Provenance => {
                assert_eq!(fixture.fixture.fixture.database.all().unwrap(), rows_before);
                assert!(
                    fixture
                        .fixture
                        .fixture
                        .database
                        .metadata_provenance(fixture.fixture.fixture.candidate_state)
                        .unwrap()
                        .is_none()
                );
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            CaptureRace::Namespace => {
                assert_eq!(fixture.fixture.fixture.database.all().unwrap(), rows_before);
                assert!(inserted.is_dir());
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FinalRace {
    Database,
    Provenance,
    Journal,
    Namespace,
}

#[test]
fn startup_activate_archived_complete_route_final_revalidation_rejects_database_provenance_journal_and_namespace_races()
{
    for race in [
        FinalRace::Database,
        FinalRace::Provenance,
        FinalRace::Journal,
        FinalRace::Namespace,
    ] {
        let fixture = exact_fixture();
        let expected = fixture.expected_successor();
        let rows_before = fixture.fixture.fixture.database.all().unwrap();
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        let inserted = fixture
            .fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("activate-archived-final-race");
        let hook: Box<dyn FnOnce()> = match race {
            FinalRace::Database => {
                let database = fixture.fixture.fixture.database.clone();
                let candidate = fixture.fixture.fixture.candidate_state;
                Box::new(move || database.remove(&candidate).unwrap())
            }
            FinalRace::Provenance => {
                let database = fixture.fixture.fixture.database.clone();
                let candidate = fixture.fixture.fixture.candidate_state;
                Box::new(move || database.delete_metadata_provenance_for_test(candidate).unwrap())
            }
            FinalRace::Journal => {
                let canonical = fixture
                    .fixture
                    .fixture
                    .installation
                    .root
                    .join(".cast/journal/state-transition");
                let changed = encode(&expected).unwrap();
                Box::new(move || fs::write(canonical, changed).unwrap())
            }
            FinalRace::Namespace => {
                let namespace_hook = fixture
                    .fixture
                    .namespace_change_hook("activate-archived-final-race".to_owned());
                Box::new(move || {
                    arm_before_usr_rollback_activate_archived_complete_route_fresh_namespace_capture(namespace_hook);
                })
            }
        };
        arm_before_usr_rollback_activate_archived_complete_route_final_revalidation(hook);
        reset_candidate_observers();

        let error = enter_route(&fixture);

        assert_complete_persistence_authority_error(&error);
        assert_eq!(candidate_move_count(), 0, "{race:?}");
        match race {
            FinalRace::Database => {
                assert_eq!(fixture.canonical_record(), fixture.source);
                assert_eq!(
                    fixture.fixture.fixture.database.all().unwrap().len(),
                    rows_before.len() - 1
                );
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            FinalRace::Provenance => {
                assert_eq!(fixture.canonical_record(), fixture.source);
                assert_eq!(fixture.fixture.fixture.database.all().unwrap(), rows_before);
                assert!(
                    fixture
                        .fixture
                        .fixture
                        .database
                        .metadata_provenance(fixture.fixture.fixture.candidate_state)
                        .unwrap()
                        .is_none()
                );
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            FinalRace::Journal => {
                assert_eq!(fixture.canonical_record(), expected);
                assert_eq!(fixture.database_snapshot(), database_before);
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            FinalRace::Namespace => {
                assert_eq!(fixture.canonical_record(), fixture.source);
                assert_eq!(fixture.database_snapshot(), database_before);
                assert!(inserted.is_dir());
            }
        }
    }
}

fn assert_capture_race_pending(error: &startup_gate::Error, race: CaptureRace) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected {race:?} capture race to remain recovery-pending, got {error:?}");
    };
    assert_eq!(pending.phase(), Phase::CandidatePreserved);
    match race {
        CaptureRace::Database => assert!(pending.blockers().contains(&RecoveryBlocker::DatabaseConflict)),
        CaptureRace::Provenance => {
            assert!(
                pending
                    .blockers()
                    .contains(&RecoveryBlocker::MetadataProvenanceConflict)
            )
        }
        CaptureRace::Namespace => {}
    }
}

fn exact_fixture() -> RouteFixture {
    RouteFixture::new(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::Applied,
    )
}
