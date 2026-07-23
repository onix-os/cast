//! Final persistence revalidation race contracts.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            arm_before_new_state_candidate_preserve_durable_post_revalidation_capture,
            arm_before_usr_rollback_candidate_preserve_durable_trailing_evidence,
        },
        startup_recovery::{
            UsrRollbackCandidatePreservePersistenceError,
            arm_before_usr_rollback_candidate_preserve_persistence_final_revalidation,
            persist_usr_rollback_candidate_preserve_and_reopen,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::support::{CandidateOrigin, Fixture, OperationKind, Source, durable_authority, fixture_for_origin};

#[derive(Clone, Copy, Debug)]
enum EvidenceRace {
    Database,
    Provenance,
    Journal,
    Installation,
    Namespace,
    Plan,
}

#[test]
fn startup_usr_rollback_candidate_preserve_persistence_rejects_mixed_and_cross_root_journals() {
    for origin in CandidateOrigin::ALL {
        let fixture = fixture_for_origin(origin, Source::Exchanged, RollbackActionOutcome::Applied);
        let first = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = durable_authority(&fixture, &first, &reservation, origin);
        drop(first);
        let second = fixture.open_journal();

        let error = persist_usr_rollback_candidate_preserve_and_reopen(second, authority).unwrap_err();

        assert!(matches!(
            error,
            UsrRollbackCandidatePreservePersistenceError::Authority(_)
        ));
        assert_eq!(fixture.fixture.canonical_record(), fixture.candidate_intent);
        drop(reservation);

        let first_fixture = fixture_for_origin(origin, Source::Intent, RollbackActionOutcome::AlreadySatisfied);
        let second_fixture = fixture_for_origin(origin, Source::Intent, RollbackActionOutcome::AlreadySatisfied);
        let first = first_fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = durable_authority(&first_fixture, &first, &reservation, origin);
        drop(first);
        fs::write(
            super::super::test_fixture::canonical_journal(&second_fixture.fixture.installation.root),
            first_fixture.fixture.canonical_bytes(),
        )
        .unwrap();
        let foreign = second_fixture.open_journal();

        let error = persist_usr_rollback_candidate_preserve_and_reopen(foreign, authority).unwrap_err();

        assert!(matches!(
            error,
            UsrRollbackCandidatePreservePersistenceError::Authority(_)
        ));
        assert_eq!(first_fixture.fixture.canonical_record(), first_fixture.candidate_intent);
        assert_eq!(
            second_fixture.fixture.canonical_record(),
            first_fixture.candidate_intent
        );
    }
}

#[test]
fn startup_usr_rollback_candidate_preserve_persistence_final_races_fail_before_advance() {
    for origin in CandidateOrigin::ALL {
        for race in [
            EvidenceRace::Database,
            EvidenceRace::Provenance,
            EvidenceRace::Journal,
            EvidenceRace::Installation,
            EvidenceRace::Namespace,
            EvidenceRace::Plan,
        ] {
            let fixture = fixture_for_origin(origin, Source::Exchanged, RollbackActionOutcome::Applied);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = durable_authority(&fixture, &journal, &reservation, origin);
            let source = fixture.candidate_intent.clone();
            arm_final_race(&fixture, race);

            let error = persist_usr_rollback_candidate_preserve_and_reopen(journal, authority).unwrap_err();

            assert!(
                matches!(error, UsrRollbackCandidatePreservePersistenceError::Authority(_)),
                "{race:?}"
            );
            if matches!(
                race,
                EvidenceRace::Database | EvidenceRace::Provenance | EvidenceRace::Namespace
            ) {
                assert_eq!(fixture.fixture.canonical_record(), source, "{race:?}");
            }
        }
    }
}

fn arm_final_race(fixture: &Fixture, race: EvidenceRace) {
    let hook: Box<dyn FnOnce()> = match race {
        EvidenceRace::Database => {
            let mutation = fixture.candidate_transition_clear_hook();
            Box::new(move || {
                arm_before_usr_rollback_candidate_preserve_durable_trailing_evidence(mutation);
            })
        }
        EvidenceRace::Provenance => {
            let database = fixture.fixture.database.clone();
            let candidate = fixture.fixture.candidate_state;
            Box::new(move || {
                arm_before_usr_rollback_candidate_preserve_durable_trailing_evidence(move || {
                    database.delete_metadata_provenance_for_test(candidate).unwrap();
                });
            })
        }
        EvidenceRace::Journal => {
            let mutation = fixture.journal_change_hook();
            Box::new(move || {
                arm_before_usr_rollback_candidate_preserve_durable_trailing_evidence(mutation);
            })
        }
        EvidenceRace::Installation => {
            let cast = fixture.fixture.installation.root.join(".cast");
            let displaced = fixture.fixture.installation.root.join(".cast-persistence-rebound");
            Box::new(move || {
                fs::rename(&cast, displaced).unwrap();
                fs::create_dir(&cast).unwrap();
                fs::set_permissions(cast, fs::Permissions::from_mode(0o700)).unwrap();
            })
        }
        EvidenceRace::Namespace => {
            let mutation = fixture.namespace_change_hook("candidate-persistence-final-namespace".to_owned());
            Box::new(move || {
                arm_before_new_state_candidate_preserve_durable_post_revalidation_capture(mutation);
            })
        }
        EvidenceRace::Plan => {
            let changed = Fixture::new(
                OperationKind::Archived,
                Source::Exchanged,
                RollbackActionOutcome::Applied,
                super::super::candidate_test_support::CandidateLayout::Staged,
            );
            let bytes = changed.fixture.canonical_bytes();
            let canonical = super::super::test_fixture::canonical_journal(&fixture.fixture.installation.root);
            Box::new(move || {
                arm_before_usr_rollback_candidate_preserve_durable_trailing_evidence(move || {
                    fs::write(canonical, bytes).unwrap();
                });
            })
        }
    };
    arm_before_usr_rollback_candidate_preserve_persistence_final_revalidation(hook);
}
