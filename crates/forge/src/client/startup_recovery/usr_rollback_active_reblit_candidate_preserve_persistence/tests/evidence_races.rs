//! Final retained-evidence revalidation contracts.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            active_reblit_candidate_preserve_exchange_attempt_count,
            arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture,
            arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
        },
        startup_recovery::{
            UsrRollbackActiveReblitCandidatePreservePersistenceError,
            arm_before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation,
            persist_usr_rollback_active_reblit_candidate_preserve_and_reopen,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::support::{CandidateOrigin, Epoch, Fixture, OperationKind, Source, durable_authority, fixture_for_origin};

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
fn startup_active_reblit_candidate_preserve_persistence_rejects_mixed_and_cross_root_journals() {
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            let fixture = fixture_for_origin(epoch, origin, Source::Exchanged, RollbackActionOutcome::Applied);
            let first = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_active_reblit_candidate_preserve_exchange_attempt_count();
            let authority = durable_authority(&fixture, &first, &reservation, origin);
            let expected_exchange_count = usize::from(origin == CandidateOrigin::Applied);
            drop(first);
            let second = fixture.open_journal();

            let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(second, authority);
            drop(reservation);
            let error = result.unwrap_err();

            assert!(matches!(
                error,
                UsrRollbackActiveReblitCandidatePreservePersistenceError::Authority(_)
            ));
            assert_eq!(fixture.fixture.canonical_record(), fixture.candidate_intent);
            assert_eq!(
                active_reblit_candidate_preserve_exchange_attempt_count(),
                expected_exchange_count
            );
            let first_fixture = fixture_for_origin(
                epoch,
                origin,
                Source::Intent,
                RollbackActionOutcome::AlreadySatisfied,
            );
            let second_fixture = fixture_for_origin(
                epoch,
                origin,
                Source::Intent,
                RollbackActionOutcome::AlreadySatisfied,
            );
            let first = first_fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_active_reblit_candidate_preserve_exchange_attempt_count();
            let authority = durable_authority(&first_fixture, &first, &reservation, origin);
            drop(first);
            fs::write(
                super::super::test_fixture::canonical_journal(&second_fixture.fixture.installation.root),
                first_fixture.fixture.canonical_bytes(),
            )
            .unwrap();
            let foreign = second_fixture.open_journal();

            let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(foreign, authority);
            drop(reservation);
            let error = result.unwrap_err();

            assert!(matches!(
                error,
                UsrRollbackActiveReblitCandidatePreservePersistenceError::Authority(_)
            ));
            assert_eq!(first_fixture.fixture.canonical_record(), first_fixture.candidate_intent);
            assert_eq!(
                second_fixture.fixture.canonical_record(),
                first_fixture.candidate_intent
            );
            assert_eq!(
                active_reblit_candidate_preserve_exchange_attempt_count(),
                expected_exchange_count
            );
        }
    }
}

#[test]
fn startup_active_reblit_candidate_preserve_persistence_final_races_fail_before_advance() {
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            for race in [
                EvidenceRace::Database,
                EvidenceRace::Provenance,
                EvidenceRace::Journal,
                EvidenceRace::Installation,
                EvidenceRace::Namespace,
                EvidenceRace::Plan,
            ] {
                let fixture = fixture_for_origin(epoch, origin, Source::Exchanged, RollbackActionOutcome::Applied);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_active_reblit_candidate_preserve_exchange_attempt_count();
                let authority = durable_authority(&fixture, &journal, &reservation, origin);
                let source = fixture.candidate_intent.clone();
                let expected_exchange_count = usize::from(origin == CandidateOrigin::Applied);
                arm_final_race(&fixture, race);

                let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, authority);
                drop(reservation);
                let error = result.unwrap_err();

                assert!(
                    matches!(
                        error,
                        UsrRollbackActiveReblitCandidatePreservePersistenceError::Authority(_)
                    ),
                    "{epoch:?} {origin:?} {race:?}"
                );
                assert_eq!(
                    active_reblit_candidate_preserve_exchange_attempt_count(),
                    expected_exchange_count,
                    "{epoch:?} {origin:?} {race:?}"
                );
                if matches!(
                    race,
                    EvidenceRace::Database | EvidenceRace::Provenance | EvidenceRace::Namespace
                ) {
                    assert_eq!(
                        fixture.fixture.canonical_record(),
                        source,
                        "{epoch:?} {origin:?} {race:?}"
                    );
                }
            }
        }
    }
}

fn arm_final_race(fixture: &Fixture, race: EvidenceRace) {
    let hook: Box<dyn FnOnce()> = match race {
        EvidenceRace::Database => {
            let database = fixture.fixture.database.clone();
            let candidate = fixture.fixture.candidate_state;
            Box::new(move || {
                arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence(move || {
                    database.remove(&candidate).unwrap();
                });
            })
        }
        EvidenceRace::Provenance => {
            let database = fixture.fixture.database.clone();
            let candidate = fixture.fixture.candidate_state;
            Box::new(move || {
                arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence(move || {
                    database.delete_metadata_provenance_for_test(candidate).unwrap();
                });
            })
        }
        EvidenceRace::Journal => {
            let mutation = fixture.journal_change_hook();
            Box::new(move || {
                arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence(mutation);
            })
        }
        EvidenceRace::Installation => {
            let cast = fixture.fixture.installation.root.join(".cast");
            let displaced = fixture
                .fixture
                .installation
                .root
                .join(".cast-active-reblit-persistence-rebound");
            Box::new(move || {
                fs::rename(&cast, displaced).unwrap();
                fs::create_dir(&cast).unwrap();
                fs::set_permissions(cast, fs::Permissions::from_mode(0o700)).unwrap();
            })
        }
        EvidenceRace::Namespace => {
            let mutation = fixture.namespace_change_hook("active-reblit-persistence-final-namespace".to_owned());
            Box::new(move || {
                arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture(mutation);
            })
        }
        EvidenceRace::Plan => {
            let changed = Fixture::new(
                OperationKind::Archived,
                super::super::candidate_test_support::CandidateSource::Exchanged,
                RollbackActionOutcome::Applied,
                super::super::candidate_test_support::CandidateLayout::Staged,
            );
            let bytes = changed.fixture.canonical_bytes();
            let canonical = super::super::test_fixture::canonical_journal(&fixture.fixture.installation.root);
            Box::new(move || {
                arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence(move || {
                    fs::write(canonical, bytes).unwrap();
                });
            })
        }
    };
    arm_before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation(hook);
}
