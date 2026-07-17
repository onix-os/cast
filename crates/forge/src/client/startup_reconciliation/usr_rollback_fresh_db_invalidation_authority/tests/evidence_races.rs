use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationAdmission, UsrRollbackFreshDbInvalidationApplyReconciliation,
            arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture,
            arm_between_usr_rollback_fresh_db_invalidation_database_captures, fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::UsrRollbackFreshDbInvalidationEffectSeal,
    },
    db::{
        self,
        state::{
            ExactFreshTransitionRemovalFault, arm_after_exact_fresh_transition_removal_attempt_before_reconciliation,
            arm_exact_fresh_transition_removal_fault, assert_exact_fresh_transition_removal_fault_consumed,
        },
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::support::{
    CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout, create_private_directory,
    transition_quarantine_path,
};

#[test]
fn startup_fresh_db_invalidation_capture_rejects_database_changes_between_its_two_snapshots() {
    let fixture = FreshDbInvalidationFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
        FreshRowLayout::Present,
    );
    let database = fixture.fixture.fixture.database.clone();
    let candidate = fixture.fixture.fixture.candidate_state;
    let transition = fixture.record.transition_id.clone();
    arm_between_usr_rollback_fresh_db_invalidation_database_captures(move || {
        let observed = database.inspect_exact_fresh_transition(candidate, &transition).unwrap();
        let db::state::ExactFreshTransitionObservation::Present(preimage) = observed else {
            panic!("race fixture must start with a complete preimage");
        };
        database.remove_exact_fresh_transition(preimage).unwrap();
    });
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();

    assert!(matches!(
        fixture.capture(&journal, &reservation).unwrap(),
        UsrRollbackFreshDbInvalidationAdmission::Deferred
    ));
    fixture.assert_exact_joint_absence();
    assert_eq!(fixture.canonical_record(), fixture.record);
    drop(journal);
    drop(reservation);

    for corrupt in [false, true] {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::AlreadySatisfied,
            FreshRowLayout::Present,
        );
        if corrupt {
            arm_between_usr_rollback_fresh_db_invalidation_database_captures(fixture.provenance_delete_hook());
        } else {
            arm_between_usr_rollback_fresh_db_invalidation_database_captures(fixture.transition_clear_hook());
        }
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(fixture.capture(&journal, &reservation).is_err());
        assert_eq!(fixture.canonical_record(), fixture.record);
    }
}

#[test]
fn startup_fresh_db_invalidation_final_database_namespace_and_journal_races_refuse_authority() {
    for race in 0..5 {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOutcome::AlreadySatisfied,
            FreshRowLayout::Present,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_apply(&journal, &reservation);
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();

        match race {
            0 => {
                let database = fixture.fixture.fixture.database.clone();
                let candidate = fixture.fixture.fixture.candidate_state;
                let transition = fixture.record.transition_id.clone();
                arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture(move || {
                    let observed = database.inspect_exact_fresh_transition(candidate, &transition).unwrap();
                    let db::state::ExactFreshTransitionObservation::Present(preimage) = observed else {
                        panic!("final DB race must start with a complete preimage");
                    };
                    database.remove_exact_fresh_transition(preimage).unwrap();
                });
            }
            1 => {
                let target = transition_quarantine_path(&fixture.fixture.fixture, &fixture.record);
                arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture(move || {
                    fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
                });
            }
            2 => arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture(fixture.journal_change_hook()),
            3 => {
                arm_exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::BeforeCommit);
                arm_after_exact_fresh_transition_removal_attempt_before_reconciliation(fixture.journal_change_hook());
            }
            4 => {
                let target = transition_quarantine_path(&fixture.fixture.fixture, &fixture.record);
                arm_exact_fresh_transition_removal_fault(
                    ExactFreshTransitionRemovalFault::AfterCommitWithUncertainReport,
                );
                arm_after_exact_fresh_transition_removal_attempt_before_reconciliation(move || {
                    fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
                });
            }
            _ => unreachable!(),
        }

        assert!(authority.reconcile(&seal, &journal).is_err(), "race={race}");
        if race >= 3 {
            assert_exact_fresh_transition_removal_fault_consumed();
        }
        assert_eq!(
            fresh_db_invalidation_removal_call_count(),
            if race >= 3 { 1 } else { 0 },
            "race={race}"
        );
        if matches!(race, 0 | 4) {
            fixture.assert_exact_joint_absence();
        } else {
            fixture.assert_exact_present();
        }
    }
}

#[test]
fn startup_fresh_db_invalidation_binding_rejects_reopened_and_cross_root_journals_before_removal() {
    let fixture = FreshDbInvalidationFixture::new(
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::Applied,
        FreshRowLayout::Present,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_apply(&journal, &reservation);
    drop(journal);
    let reopened = fixture.open_journal();
    let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
    assert!(authority.reconcile(&seal, &reopened).is_err());
    assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
    fixture.assert_exact_present();
    drop(reservation);

    let first = FreshDbInvalidationFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
        FreshRowLayout::Present,
    );
    let second = FreshDbInvalidationFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
        FreshRowLayout::Present,
    );
    second.overwrite_canonical(&first.record);
    let journal = first.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = first.capture_apply(&journal, &reservation);
    let foreign = TransitionJournalStore::open_retained(
        second.fixture.fixture.installation.root_directory(),
        &second.fixture.fixture.installation.root,
    )
    .unwrap();
    let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();

    assert!(authority.reconcile(&seal, &foreign).is_err());
    assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
    first.assert_exact_present();
    second.assert_exact_present();
}

#[test]
fn startup_fresh_db_invalidation_refuses_conflicting_lookalikes_and_retains_stable_ambient_quarantine() {
    for case in 0..3 {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOutcome::AlreadySatisfied,
            FreshRowLayout::Present,
        );
        let target = transition_quarantine_path(&fixture.fixture.fixture, &fixture.record);
        match case {
            0 => fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap(),
            1 => fs::rename(
                &target,
                fixture
                    .fixture
                    .fixture
                    .installation
                    .state_quarantine_dir()
                    .join("displaced-fresh-db-invalidation-target"),
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

        let admission = fixture.capture(&journal, &reservation);
        assert!(
            admission.is_err() || matches!(admission.unwrap(), UsrRollbackFreshDbInvalidationAdmission::Deferred),
            "namespace lookalike {case} was admitted"
        );
        fixture.assert_exact_present();
        assert_eq!(fixture.canonical_record(), fixture.record);
    }

    {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOutcome::AlreadySatisfied,
            FreshRowLayout::Present,
        );
        let ambient = fixture
            .fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("stable-fresh-db-invalidation-ambient");
        create_private_directory(&ambient);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_apply(&journal, &reservation);
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();

        assert!(matches!(
            authority.reconcile(&seal, &journal).unwrap(),
            UsrRollbackFreshDbInvalidationApplyReconciliation::Applied(_)
        ));
        assert_eq!(fresh_db_invalidation_removal_call_count(), 1);
        fixture.assert_exact_joint_absence();
    }

    {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::Applied,
            FreshRowLayout::Present,
        );
        let ambient = fixture
            .fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("changed-fresh-db-invalidation-ambient");
        create_private_directory(&ambient);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_apply(&journal, &reservation);
        fs::set_permissions(ambient, fs::Permissions::from_mode(0o500)).unwrap();
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();

        assert!(authority.reconcile(&seal, &journal).is_err());
        assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
        fixture.assert_exact_present();
    }
}
