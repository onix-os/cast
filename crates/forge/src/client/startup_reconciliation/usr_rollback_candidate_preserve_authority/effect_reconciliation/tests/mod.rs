//! Focused contracts for the sealed first NewState preservation move.

use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            NewStateCandidatePreserveMoveFault, UsrRollbackCandidatePreserveAdmission,
            UsrRollbackCandidatePreserveApplyEffectSelection, UsrRollbackNewStateCandidatePreserveApplyReconciliation,
            UsrRollbackNewStateCandidatePreserveEffectLease, arm_before_new_state_candidate_preserve_candidate_sync,
            arm_before_new_state_candidate_preserve_move_reconciliation_capture,
            arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture,
            arm_new_state_candidate_preserve_move_fault, new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
        },
        startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::super::test_fixture::OperationKind;
use super::super::test_support::{
    CandidateLayout, CandidatePreserveFixture, CandidateSource, transition_quarantine_path,
};

fn move_lease<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackNewStateCandidatePreserveEffectLease<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
        panic!("exact NewState empty-prefix evidence did not admit Apply authority");
    };
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    let UsrRollbackCandidatePreserveApplyEffectSelection::MoveNewState(lease) =
        authority.into_effect_selection(&seal, journal).unwrap()
    else {
        panic!("exact NewState empty-prefix evidence did not select the move lease");
    };
    lease
}

#[test]
fn startup_candidate_preserve_effect_selects_only_new_state_empty_quarantine_prefix() {
    let selected = CandidatePreserveFixture::new_state_empty_quarantine_prefix(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
    );
    let selected_before = selected.evidence_snapshots();
    let journal = selected.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_new_state_candidate_preserve_move_attempt_count();
    let lease = move_lease(&selected, &journal, &reservation);
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    drop(lease);
    selected.assert_evidence_unchanged(&selected_before);
    drop(reservation);
    drop(journal);

    for fixture in [
        CandidatePreserveFixture::new(
            OperationKind::NewState,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateLayout::Staged,
        ),
        CandidatePreserveFixture::new(
            OperationKind::Archived,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateLayout::Staged,
        ),
        CandidatePreserveFixture::new(
            OperationKind::ActiveReblit,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateLayout::Staged,
        )
        .with_active_reblit_wrapper_index(7),
    ] {
        let before = fixture.evidence_snapshots();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
            panic!("unsupported exact staged evidence did not admit Apply authority");
        };
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
        reset_new_state_candidate_preserve_move_attempt_count();

        assert!(matches!(
            authority.into_effect_selection(&seal, &journal).unwrap(),
            UsrRollbackCandidatePreserveApplyEffectSelection::Unsupported
        ));
        assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
        fixture.assert_evidence_unchanged(&before);
    }
}

#[test]
fn startup_new_state_candidate_preserve_move_reconciles_every_raw_result_for_every_origin() {
    let cases = [
        (None, true),
        (Some(NewStateCandidatePreserveMoveFault::ErrorAfterApply), true),
        (Some(NewStateCandidatePreserveMoveFault::ErrorWithoutApply), false),
        (Some(NewStateCandidatePreserveMoveFault::SuccessWithoutApply), false),
    ];

    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for (fault, expected_applied) in cases {
                let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(source, usr_outcome);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let lease = move_lease(&fixture, &journal, &reservation);
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
                reset_new_state_candidate_preserve_move_attempt_count();
                match fault {
                    Some(fault) => arm_new_state_candidate_preserve_move_fault(fault),
                    None => {}
                }

                let result = lease.reconcile(&seal, &journal).unwrap();

                assert_eq!(
                    new_state_candidate_preserve_move_attempt_count(),
                    1,
                    "{source:?} {usr_outcome:?} {fault:?}"
                );
                match (expected_applied, result) {
                    (true, UsrRollbackNewStateCandidatePreserveApplyReconciliation::Applied(authority)) => {
                        drop(authority);
                        assert!(matches!(
                            fixture.capture(&journal, &reservation),
                            UsrRollbackCandidatePreserveAdmission::Finish(_)
                        ));
                    }
                    (false, UsrRollbackNewStateCandidatePreserveApplyReconciliation::NotApplied) => {
                        assert!(matches!(
                            fixture.capture(&journal, &reservation),
                            UsrRollbackCandidatePreserveAdmission::Apply(_)
                        ));
                    }
                    (_, UsrRollbackNewStateCandidatePreserveApplyReconciliation::Ambiguous) => {
                        panic!("stable {source:?} {usr_outcome:?} {fault:?} evidence was ambiguous");
                    }
                    (true, UsrRollbackNewStateCandidatePreserveApplyReconciliation::NotApplied) => {
                        panic!("applied {source:?} {usr_outcome:?} {fault:?} move was reported unapplied");
                    }
                    (false, UsrRollbackNewStateCandidatePreserveApplyReconciliation::Applied(_)) => {
                        panic!("unapplied {source:?} {usr_outcome:?} {fault:?} move was reported applied");
                    }
                }
                fixture.assert_non_namespace_unchanged();
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PostAttemptChange {
    ExtraNamespaceEntry,
    CandidateMarkerRemoval,
}

#[test]
fn startup_new_state_candidate_preserve_move_ambiguity_consumes_all_retry_capability() {
    for change in [
        PostAttemptChange::ExtraNamespaceEntry,
        PostAttemptChange::CandidateMarkerRemoval,
    ] {
        let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = move_lease(&fixture, &journal, &reservation);
        match change {
            PostAttemptChange::ExtraNamespaceEntry => {
                arm_before_new_state_candidate_preserve_move_reconciliation_capture(
                    fixture.namespace_change_hook("candidate-preserve-post-attempt-delta".to_owned()),
                );
            }
            PostAttemptChange::CandidateMarkerRemoval => {
                let marker =
                    transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent).join("usr/.cast-tree-id");
                arm_before_new_state_candidate_preserve_move_reconciliation_capture(move || {
                    fs::remove_file(marker).unwrap();
                });
            }
        }
        reset_new_state_candidate_preserve_move_attempt_count();
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(matches!(
            lease.reconcile(&seal, &journal).unwrap(),
            UsrRollbackNewStateCandidatePreserveApplyReconciliation::Ambiguous
        ));
        assert_eq!(new_state_candidate_preserve_move_attempt_count(), 1, "{change:?}");
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_new_state_candidate_preserve_move_final_prefix_race_prevents_the_attempt() {
    let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = move_lease(&fixture, &journal, &reservation);
    arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture(
        fixture.namespace_change_hook("candidate-preserve-final-prefix-race".to_owned()),
    );
    reset_new_state_candidate_preserve_move_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_new_state_candidate_preserve_effect_selection_starts_with_the_open_binding() {
    let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
    );
    let first = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&first, &reservation) else {
        panic!("exact NewState empty-prefix evidence did not admit Apply authority");
    };
    drop(first);
    let second = fixture.open_journal();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    reset_new_state_candidate_preserve_move_attempt_count();

    assert!(authority.into_effect_selection(&seal, &second).is_err());
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_new_state_candidate_preserve_move_consumption_starts_with_the_open_binding() {
    let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
    );
    let first = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = move_lease(&fixture, &first, &reservation);
    drop(first);
    let second = fixture.open_journal();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    reset_new_state_candidate_preserve_move_attempt_count();

    assert!(lease.reconcile(&seal, &second).is_err());
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    fixture.assert_non_namespace_unchanged();
}

#[derive(Clone, Copy, Debug)]
enum TrailingEvidenceChange {
    Database,
    Journal,
}

#[test]
fn startup_new_state_candidate_preserve_move_rechecks_database_and_journal_after_namespace_use() {
    for change in [TrailingEvidenceChange::Database, TrailingEvidenceChange::Journal] {
        let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = move_lease(&fixture, &journal, &reservation);
        let hook: Box<dyn FnOnce()> = match change {
            TrailingEvidenceChange::Database => Box::new(fixture.candidate_transition_clear_hook()),
            TrailingEvidenceChange::Journal => Box::new(fixture.journal_change_hook()),
        };
        arm_before_new_state_candidate_preserve_move_reconciliation_capture(hook);
        reset_new_state_candidate_preserve_move_attempt_count();
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(lease.reconcile(&seal, &journal).is_err(), "{change:?}");
        assert_eq!(new_state_candidate_preserve_move_attempt_count(), 1, "{change:?}");
        assert!(!fixture.fixture.installation.staging_dir().join("usr").exists());
        assert!(
            transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent)
                .join("usr")
                .is_dir()
        );
    }
}

#[test]
fn startup_new_state_candidate_preserve_move_candidate_presync_race_prevents_the_attempt() {
    let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = move_lease(&fixture, &journal, &reservation);
    let marker = fixture.fixture.installation.staging_dir().join("usr/.cast-tree-id");
    arm_before_new_state_candidate_preserve_candidate_sync(move || {
        fs::remove_file(marker).unwrap();
    });
    reset_new_state_candidate_preserve_move_attempt_count();
    let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    assert!(fixture.fixture.installation.staging_dir().join("usr").is_dir());
    assert!(
        !transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent)
            .join("usr")
            .exists()
    );
    fixture.assert_non_namespace_unchanged();
}
