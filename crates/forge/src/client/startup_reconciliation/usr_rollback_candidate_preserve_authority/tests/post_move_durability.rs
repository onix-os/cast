//! Shared NewState durability after an applied or already-preserved move.

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            NewStateCandidatePreserveMoveFault, NewStateCandidatePreservePostMoveDurabilityEvent,
            NewStateCandidatePreservePostMoveDurabilityFaultPoint, UsrRollbackCandidatePreserveAdmission,
            UsrRollbackCandidatePreserveApplyEffectSelection, UsrRollbackCandidatePreserveFinishDurabilitySelection,
            UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority,
            UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority,
            UsrRollbackNewStateCandidatePreserveApplyReconciliation,
            UsrRollbackNewStateCandidatePreserveDurableEffectAuthority,
            arm_before_new_state_candidate_preserve_post_move_candidate_sync,
            arm_before_new_state_candidate_preserve_post_move_final_post_capture,
            arm_before_new_state_candidate_preserve_post_move_quarantine_parent_sync,
            arm_before_new_state_candidate_preserve_post_move_staging_parent_sync,
            arm_before_new_state_candidate_preserve_post_move_target_parent_sync,
            arm_new_state_candidate_preserve_move_fault, arm_new_state_candidate_preserve_post_move_durability_fault,
            new_state_candidate_preserve_move_attempt_count, reset_new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_post_move_durability_events,
            take_new_state_candidate_preserve_post_move_durability_events,
        },
        startup_recovery::{UsrRollbackCandidatePreserveDurabilitySeal, UsrRollbackCandidatePreserveEffectSeal},
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::{
    fixture::OperationKind,
    support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, transition_quarantine_path},
};

fn reconcile_applied<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    fault: Option<NewStateCandidatePreserveMoveFault>,
) -> UsrRollbackNewStateCandidatePreserveAppliedEffectAuthority<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
        panic!("exact NewState move prefix did not admit Apply authority");
    };
    let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    let UsrRollbackCandidatePreserveApplyEffectSelection::MoveNewState(lease) =
        authority.into_effect_selection(&effect_seal, journal).unwrap()
    else {
        panic!("exact NewState move prefix did not select the move lease");
    };
    if let Some(fault) = fault {
        arm_new_state_candidate_preserve_move_fault(fault);
    }
    let UsrRollbackNewStateCandidatePreserveApplyReconciliation::Applied(authority) =
        lease.reconcile(&effect_seal, journal).unwrap()
    else {
        panic!("applied NewState move did not reconcile as Applied");
    };
    authority
}

fn select_finish<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackNewStateCandidatePreserveAlreadySatisfiedEffectAuthority<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(journal, reservation) else {
        panic!("exact NewState POST did not admit Finish authority");
    };
    let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    let UsrRollbackCandidatePreserveFinishDurabilitySelection::NewState(authority) = authority
        .into_post_move_durability_selection(&effect_seal, journal)
        .unwrap()
    else {
        panic!("exact NewState Finish authority did not select durability");
    };
    authority
}

fn expected_events(fixture: &CandidatePreserveFixture) -> Vec<NewStateCandidatePreservePostMoveDurabilityEvent> {
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let candidate = identity(&target.join("usr"));
    let staging_parent = identity(&fixture.fixture.installation.staging_dir());
    let target_parent = identity(&target);
    let quarantine_parent = identity(&fixture.fixture.installation.state_quarantine_dir());
    vec![
        NewStateCandidatePreservePostMoveDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::StagingParentSynced {
            device: staging_parent.0,
            inode: staging_parent.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::TargetParentSynced {
            device: target_parent.0,
            inode: target_parent.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::QuarantineParentSynced {
            device: quarantine_parent.0,
            inode: quarantine_parent.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::FinalPostProven,
    ]
}

fn reset_observations() {
    reset_new_state_candidate_preserve_move_attempt_count();
    reset_new_state_candidate_preserve_post_move_durability_events();
}

fn assert_durable_origin(
    authority: UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'_>,
    expected: RollbackActionOutcome,
) {
    assert_eq!(authority.origin_for_test(), expected);
}

fn assert_fresh_finish_completes(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &ActiveStateReservation,
) {
    let seal = UsrRollbackCandidatePreserveDurabilitySeal::new_for_test();
    let authority = select_finish(fixture, journal, reservation);
    let expected = expected_events(fixture);
    reset_new_state_candidate_preserve_post_move_durability_events();
    let durable = authority.complete_post_move_durability(&seal, journal).unwrap();
    assert_eq!(
        take_new_state_candidate_preserve_post_move_durability_events(),
        expected
    );
    assert_durable_origin(durable, RollbackActionOutcome::AlreadySatisfied);
}

#[test]
fn startup_new_state_post_move_durability_orders_exact_events_for_applied_and_finish_matrices() {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(source, usr_outcome);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_observations();
            let authority = reconcile_applied(&fixture, &journal, &reservation, None);
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 1);
            let expected = expected_events(&fixture);
            reset_new_state_candidate_preserve_post_move_durability_events();
            let seal = UsrRollbackCandidatePreserveDurabilitySeal::new_for_test();

            let durable = authority.complete_post_move_durability(&seal, &journal).unwrap();

            assert_eq!(
                take_new_state_candidate_preserve_post_move_durability_events(),
                expected
            );
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 1);
            assert_durable_origin(durable, RollbackActionOutcome::Applied);
            fixture.assert_non_namespace_unchanged();
            drop(reservation);
            drop(journal);

            let fixture =
                CandidatePreserveFixture::new(OperationKind::NewState, source, usr_outcome, CandidateLayout::Preserved);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_observations();
            let seal = UsrRollbackCandidatePreserveDurabilitySeal::new_for_test();
            let authority = select_finish(&fixture, &journal, &reservation);
            let expected = expected_events(&fixture);

            let durable = authority.complete_post_move_durability(&seal, &journal).unwrap();

            assert_eq!(
                take_new_state_candidate_preserve_post_move_durability_events(),
                expected
            );
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
            assert_durable_origin(durable, RollbackActionOutcome::AlreadySatisfied);
            fixture.assert_non_namespace_unchanged();
        }
    }
}

#[test]
fn startup_new_state_post_move_durability_faults_stop_at_exact_prefixes_and_fresh_admission_repeats() {
    let cases = [
        (NewStateCandidatePreservePostMoveDurabilityFaultPoint::CandidateSync, 0),
        (
            NewStateCandidatePreservePostMoveDurabilityFaultPoint::StagingParentSync,
            1,
        ),
        (
            NewStateCandidatePreservePostMoveDurabilityFaultPoint::TargetParentSync,
            2,
        ),
        (
            NewStateCandidatePreservePostMoveDurabilityFaultPoint::QuarantineParentSync,
            3,
        ),
        (
            NewStateCandidatePreservePostMoveDurabilityFaultPoint::FinalPostCapture,
            4,
        ),
    ];
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for (fault, prefix_len) in cases {
                let fixture = CandidatePreserveFixture::new(
                    OperationKind::NewState,
                    source,
                    usr_outcome,
                    CandidateLayout::Preserved,
                );
                let before = fixture.evidence_snapshots();
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let seal = UsrRollbackCandidatePreserveDurabilitySeal::new_for_test();
                let authority = select_finish(&fixture, &journal, &reservation);
                let expected = expected_events(&fixture);
                reset_observations();
                arm_new_state_candidate_preserve_post_move_durability_fault(fault);

                assert!(authority.complete_post_move_durability(&seal, &journal).is_err());

                assert_eq!(
                    take_new_state_candidate_preserve_post_move_durability_events(),
                    expected[..prefix_len],
                    "{source:?} {usr_outcome:?} {fault:?}"
                );
                assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
                fixture.assert_evidence_unchanged(&before);

                assert_fresh_finish_completes(&fixture, &journal, &reservation);
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PostMoveRace {
    CandidateMutation,
    CandidateReplacement,
    CandidateMarkerRemoval,
    StagingPublicNameRebind,
    TargetReplacement,
    QuarantineParentRebind,
    FinalTargetMode,
    FinalExtraEntry,
}

#[test]
fn startup_new_state_post_move_durability_rejects_exact_post_races_at_every_barrier() {
    let cases = [
        (PostMoveRace::CandidateMutation, 0),
        (PostMoveRace::CandidateReplacement, 1),
        (PostMoveRace::CandidateMarkerRemoval, 1),
        (PostMoveRace::StagingPublicNameRebind, 1),
        (PostMoveRace::TargetReplacement, 2),
        (PostMoveRace::QuarantineParentRebind, 3),
        (PostMoveRace::FinalTargetMode, 4),
        (PostMoveRace::FinalExtraEntry, 4),
    ];
    for (race, prefix_len) in cases {
        let fixture = CandidatePreserveFixture::new(
            OperationKind::NewState,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateLayout::Preserved,
        );
        let before = fixture.evidence_snapshots();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let seal = UsrRollbackCandidatePreserveDurabilitySeal::new_for_test();
        let authority = select_finish(&fixture, &journal, &reservation);
        let expected = expected_events(&fixture);
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        let candidate = target.join("usr");
        reset_observations();
        match race {
            PostMoveRace::CandidateMutation => {
                arm_before_new_state_candidate_preserve_post_move_candidate_sync(move || {
                    fs::write(candidate.join("post-move-race"), b"changed").unwrap();
                });
            }
            PostMoveRace::CandidateReplacement => {
                let displaced = target.join("usr-post-move-replaced");
                arm_before_new_state_candidate_preserve_post_move_staging_parent_sync(move || {
                    fs::rename(&candidate, displaced).unwrap();
                    super::fixture::create_private_directory(&candidate);
                });
            }
            PostMoveRace::CandidateMarkerRemoval => {
                arm_before_new_state_candidate_preserve_post_move_staging_parent_sync(move || {
                    fs::remove_file(candidate.join(".cast-tree-id")).unwrap();
                });
            }
            PostMoveRace::StagingPublicNameRebind => {
                let staging = fixture.fixture.installation.staging_dir();
                let displaced = staging.with_file_name("staging-post-move-rebound");
                arm_before_new_state_candidate_preserve_post_move_staging_parent_sync(move || {
                    fs::rename(&staging, displaced).unwrap();
                    super::fixture::create_private_directory(&staging);
                });
            }
            PostMoveRace::TargetReplacement => {
                let displaced = target.with_file_name("candidate-target-post-move-replaced");
                arm_before_new_state_candidate_preserve_post_move_target_parent_sync(move || {
                    fs::rename(&target, displaced).unwrap();
                    super::fixture::create_private_directory(&target);
                });
            }
            PostMoveRace::QuarantineParentRebind => {
                let quarantine = fixture.fixture.installation.state_quarantine_dir();
                let displaced = quarantine.with_file_name("quarantine-post-move-rebound");
                arm_before_new_state_candidate_preserve_post_move_quarantine_parent_sync(move || {
                    fs::rename(&quarantine, displaced).unwrap();
                    super::fixture::create_private_directory(&quarantine);
                });
            }
            PostMoveRace::FinalTargetMode => {
                arm_before_new_state_candidate_preserve_post_move_final_post_capture(move || {
                    fs::set_permissions(target, fs::Permissions::from_mode(0o755)).unwrap();
                });
            }
            PostMoveRace::FinalExtraEntry => {
                arm_before_new_state_candidate_preserve_post_move_final_post_capture(
                    fixture.namespace_change_hook("post-move-final-extra-entry".to_owned()),
                );
            }
        }

        assert!(authority.complete_post_move_durability(&seal, &journal).is_err());

        assert_eq!(
            take_new_state_candidate_preserve_post_move_durability_events(),
            expected[..prefix_len],
            "{race:?}"
        );
        assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0, "{race:?}");
        assert_eq!(fixture.fixture.canonical_bytes(), before.0, "{race:?}");
        assert_eq!(fixture.fixture.database_snapshot(), before.1, "{race:?}");
    }
}

#[derive(Clone, Copy, Debug)]
enum EvidenceRace {
    Database,
    Journal,
    Plan,
}

#[test]
fn startup_new_state_post_move_durability_rejects_evidence_races_and_fresh_admission_reruns() {
    let cases = [
        (EvidenceRace::Database, 5),
        (EvidenceRace::Journal, 5),
        (EvidenceRace::Plan, 5),
    ];
    for (race, prefix_len) in cases {
        let fixture = CandidatePreserveFixture::new(
            OperationKind::NewState,
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateLayout::Preserved,
        );
        let before = fixture.evidence_snapshots();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let seal = UsrRollbackCandidatePreserveDurabilitySeal::new_for_test();
        let authority = select_finish(&fixture, &journal, &reservation);
        let expected = expected_events(&fixture);
        reset_observations();
        let removed_provenance = match race {
            EvidenceRace::Database => {
                let database = fixture.fixture.database.clone();
                let candidate = fixture.fixture.candidate_state;
                let provenance = database.metadata_provenance(candidate).unwrap().unwrap();
                arm_before_new_state_candidate_preserve_post_move_candidate_sync(move || {
                    database.delete_metadata_provenance_for_test(candidate).unwrap();
                });
                Some(provenance)
            }
            EvidenceRace::Journal => {
                arm_before_new_state_candidate_preserve_post_move_target_parent_sync(fixture.journal_change_hook());
                None
            }
            EvidenceRace::Plan => {
                let canonical = super::fixture::canonical_journal(&fixture.fixture.installation.root);
                let changed = CandidatePreserveFixture::new(
                    OperationKind::Archived,
                    CandidateSource::Exchanged,
                    RollbackActionOutcome::Applied,
                    CandidateLayout::Staged,
                );
                let bytes = changed.fixture.canonical_bytes();
                arm_before_new_state_candidate_preserve_post_move_final_post_capture(move || {
                    fs::write(canonical, bytes).unwrap();
                });
                None
            }
        };

        assert!(authority.complete_post_move_durability(&seal, &journal).is_err());

        assert_eq!(
            take_new_state_candidate_preserve_post_move_durability_events(),
            expected[..prefix_len],
            "{race:?}"
        );
        assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0, "{race:?}");
        assert_eq!(fixture.fixture.namespace_snapshot(), before.2, "{race:?}");

        match race {
            EvidenceRace::Database => {
                fixture
                    .fixture
                    .database
                    .insert_fresh_metadata_provenance_if_transition_matches(
                        fixture.fixture.candidate_state,
                        &fixture.candidate_intent.transition_id,
                        &removed_provenance.unwrap(),
                    )
                    .unwrap();
                assert_fresh_finish_completes(&fixture, &journal, &reservation);
            }
            EvidenceRace::Journal | EvidenceRace::Plan => {
                fs::write(
                    super::fixture::canonical_journal(&fixture.fixture.installation.root),
                    &before.0,
                )
                .unwrap();
                assert_fresh_finish_completes(&fixture, &journal, &reservation);
            }
        }
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_new_state_post_move_durability_converges_applied_error_after_apply_and_finish_origins() {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for fault in [None, Some(NewStateCandidatePreserveMoveFault::ErrorAfterApply)] {
                let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                reset_observations();
                let authority = reconcile_applied(&fixture, &journal, &reservation, fault);
                let expected = expected_events(&fixture);
                reset_new_state_candidate_preserve_post_move_durability_events();
                let seal = UsrRollbackCandidatePreserveDurabilitySeal::new_for_test();

                let durable = authority.complete_post_move_durability(&seal, &journal).unwrap();

                assert_eq!(
                    take_new_state_candidate_preserve_post_move_durability_events(),
                    expected
                );
                assert_eq!(new_state_candidate_preserve_move_attempt_count(), 1);
                assert_durable_origin(durable, RollbackActionOutcome::Applied);
                fixture.assert_non_namespace_unchanged();
            }

            let fixture =
                CandidatePreserveFixture::new(OperationKind::NewState, source, usr_outcome, CandidateLayout::Preserved);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_observations();
            assert_fresh_finish_completes(&fixture, &journal, &reservation);
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
            fixture.assert_non_namespace_unchanged();
        }
    }
}

#[test]
fn startup_archived_finish_selects_its_separate_durability_authority_without_new_state_events() {
    for kind in [OperationKind::Archived] {
        for source in CandidateSource::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                let fixture = CandidatePreserveFixture::new(kind, source, usr_outcome, CandidateLayout::Preserved);
                let before = fixture.evidence_snapshots();
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(&journal, &reservation)
                else {
                    panic!("exact {kind:?} POST did not admit Finish authority");
                };
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
                reset_observations();

                let UsrRollbackCandidatePreserveFinishDurabilitySelection::Archived(authority) =
                    authority.into_post_move_durability_selection(&seal, &journal).unwrap()
                else {
                    panic!("exact archived POST did not select archived durability")
                };
                drop(authority);
                assert!(take_new_state_candidate_preserve_post_move_durability_events().is_empty());
                assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
                fixture.assert_evidence_unchanged(&before);
            }
        }
    }
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}
