//! Applied/already-satisfied archived POST durability contracts.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            ArchivedCandidatePreservePostMoveDurabilityEvent, ArchivedCandidatePreservePostMoveDurabilityFaultPoint,
            UsrRollbackCandidatePreserveAdmission, archived_candidate_preserve_move_attempt_count,
            arm_archived_candidate_preserve_post_move_durability_fault,
            arm_before_archived_candidate_preserve_durable_post_revalidation_capture,
            arm_before_archived_candidate_preserve_post_candidate_sync,
            arm_before_archived_candidate_preserve_post_final_capture,
            arm_before_archived_candidate_preserve_post_roots_parent_sync,
            arm_before_archived_candidate_preserve_post_staging_parent_sync,
            arm_before_archived_candidate_preserve_post_target_parent_sync,
            reset_archived_candidate_preserve_post_move_durability_events,
            take_archived_candidate_preserve_post_move_durability_events,
        },
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::super::super::{
    UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority,
    UsrRollbackArchivedCandidatePreserveDurableEffectAuthority, UsrRollbackArchivedCandidatePreserveEffectSeal,
};
use super::{
    FixtureEpoch, archived_fixture, archived_fixture_at_epoch, assert_preserved, identity, reconcile_applied,
    reset_observations, target_path,
};
use crate::client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::support::{
    CandidateLayout, CandidatePreserveFixture, CandidateSource,
};

fn select_finish<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackArchivedCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(journal, reservation) else {
        panic!("exact archived preserved evidence did not admit Finish")
    };
    let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();
    authority.into_archived_finish_for_test(&seal, journal).unwrap()
}

fn expected_post_events(fixture: &CandidatePreserveFixture) -> Vec<ArchivedCandidatePreservePostMoveDurabilityEvent> {
    let target = target_path(fixture);
    let candidate = identity(target.join("usr"));
    let staging = identity(fixture.fixture.installation.staging_dir());
    let target_parent = identity(&target);
    let roots = identity(fixture.fixture.installation.root.join(".cast/root"));
    vec![
        ArchivedCandidatePreservePostMoveDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::StagingParentSynced {
            device: staging.0,
            inode: staging.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::TargetParentSynced {
            device: target_parent.0,
            inode: target_parent.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::RootsParentSynced {
            device: roots.0,
            inode: roots.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::FinalPostProven,
    ]
}

fn assert_durable(
    durable: UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'_>,
    journal: &TransitionJournalStore,
) {
    durable.revalidate_for_test(journal).unwrap();
}

#[test]
fn startup_archived_post_durability_has_one_exact_order_for_applied_and_already_satisfied_matrices() {
    for epoch in FixtureEpoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = archived_fixture_at_epoch(epoch, source, usr_outcome, CandidateLayout::Staged);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_observations();
                let authority = reconcile_applied(&fixture, &journal, &reservation);
                let expected = expected_post_events(&fixture);
                reset_archived_candidate_preserve_post_move_durability_events();
                let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();

                let durable = authority.complete_post_move_durability(&seal, &journal).unwrap();

                assert_eq!(take_archived_candidate_preserve_post_move_durability_events(), expected,);
                assert_eq!(archived_candidate_preserve_move_attempt_count(), 1);
                assert_durable(durable, &journal);
                assert_preserved(&fixture);
                fixture.assert_non_namespace_unchanged();
                drop(reservation);
                drop(journal);

                let fixture = archived_fixture_at_epoch(epoch, source, usr_outcome, CandidateLayout::Preserved);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_observations();
                reset_archived_candidate_preserve_post_move_durability_events();
                let authority = select_finish(&fixture, &journal, &reservation);
                let expected = expected_post_events(&fixture);

                let durable = authority.complete_post_move_durability(&seal, &journal).unwrap();

                assert_eq!(take_archived_candidate_preserve_post_move_durability_events(), expected,);
                assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
                assert_durable(durable, &journal);
                assert_preserved(&fixture);
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

#[test]
fn startup_archived_post_faults_stop_at_exact_prefixes_for_both_origins() {
    let cases = [
        (ArchivedCandidatePreservePostMoveDurabilityFaultPoint::CandidateSync, 0),
        (
            ArchivedCandidatePreservePostMoveDurabilityFaultPoint::StagingParentSync,
            1,
        ),
        (
            ArchivedCandidatePreservePostMoveDurabilityFaultPoint::TargetParentSync,
            2,
        ),
        (
            ArchivedCandidatePreservePostMoveDurabilityFaultPoint::RootsParentSync,
            3,
        ),
        (
            ArchivedCandidatePreservePostMoveDurabilityFaultPoint::FinalPostCapture,
            4,
        ),
    ];
    for applied in [true, false] {
        for (fault, prefix_len) in cases {
            let fixture = archived_fixture(
                CandidateSource::Exchanged,
                RollbackActionOutcome::Applied,
                if applied {
                    CandidateLayout::Staged
                } else {
                    CandidateLayout::Preserved
                },
            );
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_observations();
            let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();
            if applied {
                let authority = reconcile_applied(&fixture, &journal, &reservation);
                let expected = expected_post_events(&fixture);
                reset_archived_candidate_preserve_post_move_durability_events();
                arm_archived_candidate_preserve_post_move_durability_fault(fault);
                assert!(authority.complete_post_move_durability(&seal, &journal).is_err());
                assert_eq!(
                    take_archived_candidate_preserve_post_move_durability_events(),
                    expected[..prefix_len],
                );
                assert_eq!(archived_candidate_preserve_move_attempt_count(), 1);
            } else {
                let authority = select_finish(&fixture, &journal, &reservation);
                let expected = expected_post_events(&fixture);
                reset_archived_candidate_preserve_post_move_durability_events();
                arm_archived_candidate_preserve_post_move_durability_fault(fault);
                assert!(authority.complete_post_move_durability(&seal, &journal).is_err());
                assert_eq!(
                    take_archived_candidate_preserve_post_move_durability_events(),
                    expected[..prefix_len],
                );
                assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
            }
            assert_preserved(&fixture);
            fixture.assert_non_namespace_unchanged();

            let expected = expected_post_events(&fixture);
            reset_archived_candidate_preserve_post_move_durability_events();
            let fresh = select_finish(&fixture, &journal, &reservation)
                .complete_post_move_durability(&seal, &journal)
                .unwrap();
            assert_eq!(take_archived_candidate_preserve_post_move_durability_events(), expected,);
            assert_durable(fresh, &journal);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PostRaceBoundary {
    Candidate,
    Staging,
    Target,
    Roots,
    FinalCapture,
}

impl PostRaceBoundary {
    const CASES: [(Self, usize); 5] = [
        (Self::Candidate, 0),
        (Self::Staging, 1),
        (Self::Target, 2),
        (Self::Roots, 3),
        (Self::FinalCapture, 4),
    ];

    fn arm(self, hook: impl FnOnce() + 'static) {
        match self {
            Self::Candidate => arm_before_archived_candidate_preserve_post_candidate_sync(hook),
            Self::Staging => arm_before_archived_candidate_preserve_post_staging_parent_sync(hook),
            Self::Target => arm_before_archived_candidate_preserve_post_target_parent_sync(hook),
            Self::Roots => arm_before_archived_candidate_preserve_post_roots_parent_sync(hook),
            Self::FinalCapture => arm_before_archived_candidate_preserve_post_final_capture(hook),
        }
    }
}

#[test]
fn startup_archived_post_races_fail_at_every_boundary_for_both_origins() {
    for applied in [true, false] {
        for (boundary, prefix_len) in PostRaceBoundary::CASES {
            let fixture = archived_fixture(
                CandidateSource::Exchanged,
                RollbackActionOutcome::Applied,
                if applied {
                    CandidateLayout::Staged
                } else {
                    CandidateLayout::Preserved
                },
            );
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_observations();
            let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();
            if applied {
                let authority = reconcile_applied(&fixture, &journal, &reservation);
                let expected = expected_post_events(&fixture);
                reset_archived_candidate_preserve_post_move_durability_events();
                boundary.arm(fixture.namespace_change_hook(format!("archived-applied-post-{boundary:?}")));
                assert!(authority.complete_post_move_durability(&seal, &journal).is_err());
                assert_eq!(
                    take_archived_candidate_preserve_post_move_durability_events(),
                    expected[..prefix_len],
                );
                assert_eq!(archived_candidate_preserve_move_attempt_count(), 1);
            } else {
                let authority = select_finish(&fixture, &journal, &reservation);
                let expected = expected_post_events(&fixture);
                reset_archived_candidate_preserve_post_move_durability_events();
                boundary.arm(fixture.namespace_change_hook(format!("archived-finish-post-{boundary:?}")));
                assert!(authority.complete_post_move_durability(&seal, &journal).is_err());
                assert_eq!(
                    take_archived_candidate_preserve_post_move_durability_events(),
                    expected[..prefix_len],
                );
                assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
            }
            assert_preserved(&fixture);
            fixture.assert_non_namespace_unchanged();
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PostAuthorityRace {
    Journal,
    DatabaseOwnership,
    Provenance,
}

impl PostAuthorityRace {
    const ALL: [Self; 3] = [Self::Journal, Self::DatabaseOwnership, Self::Provenance];

    fn arm(self, fixture: &CandidatePreserveFixture) {
        match self {
            Self::Journal => arm_before_archived_candidate_preserve_post_final_capture(fixture.journal_change_hook()),
            Self::DatabaseOwnership => {
                let database = fixture.fixture.database.clone();
                let transition = fixture.candidate_intent.transition_id.clone();
                arm_before_archived_candidate_preserve_post_final_capture(move || {
                    database
                        .add_with_transition(&transition, &[], Some("archived POST authority race"), None)
                        .unwrap();
                });
            }
            Self::Provenance => {
                let database = fixture.fixture.database.clone();
                let candidate = fixture.fixture.candidate_state;
                arm_before_archived_candidate_preserve_post_final_capture(move || {
                    database.delete_metadata_provenance_for_test(candidate).unwrap();
                });
            }
        }
    }
}

#[test]
fn startup_archived_post_authority_rejects_journal_database_and_provenance_races_without_another_move() {
    for applied in [true, false] {
        for race in PostAuthorityRace::ALL {
            let fixture = archived_fixture(
                CandidateSource::Exchanged,
                RollbackActionOutcome::Applied,
                if applied {
                    CandidateLayout::Staged
                } else {
                    CandidateLayout::Preserved
                },
            );
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_observations();
            let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();
            if applied {
                let authority = reconcile_applied(&fixture, &journal, &reservation);
                let expected = expected_post_events(&fixture);
                reset_archived_candidate_preserve_post_move_durability_events();
                race.arm(&fixture);

                assert!(
                    authority.complete_post_move_durability(&seal, &journal).is_err(),
                    "{race:?}"
                );

                assert_eq!(take_archived_candidate_preserve_post_move_durability_events(), expected,);
                assert_eq!(archived_candidate_preserve_move_attempt_count(), 1, "{race:?}");
            } else {
                let authority = select_finish(&fixture, &journal, &reservation);
                let expected = expected_post_events(&fixture);
                reset_archived_candidate_preserve_post_move_durability_events();
                race.arm(&fixture);

                assert!(
                    authority.complete_post_move_durability(&seal, &journal).is_err(),
                    "{race:?}"
                );

                assert_eq!(take_archived_candidate_preserve_post_move_durability_events(), expected,);
                assert_eq!(archived_candidate_preserve_move_attempt_count(), 0, "{race:?}");
            }
            assert_preserved(&fixture);
        }
    }
}

#[test]
fn startup_archived_durable_revalidation_is_fresh_and_never_repeats_barriers() {
    for applied in [true, false] {
        let fixture = archived_fixture(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            if applied {
                CandidateLayout::Staged
            } else {
                CandidateLayout::Preserved
            },
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_observations();
        let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();
        let durable = if applied {
            reconcile_applied(&fixture, &journal, &reservation)
                .complete_post_move_durability(&seal, &journal)
                .unwrap()
        } else {
            select_finish(&fixture, &journal, &reservation)
                .complete_post_move_durability(&seal, &journal)
                .unwrap()
        };
        reset_archived_candidate_preserve_post_move_durability_events();
        arm_before_archived_candidate_preserve_durable_post_revalidation_capture(
            fixture.namespace_change_hook(format!("archived-durable-race-{applied}")),
        );

        assert!(durable.revalidate_for_test(&journal).is_err());

        assert!(take_archived_candidate_preserve_post_move_durability_events().is_empty());
        assert_preserved(&fixture);
        fixture.assert_non_namespace_unchanged();
    }
}
