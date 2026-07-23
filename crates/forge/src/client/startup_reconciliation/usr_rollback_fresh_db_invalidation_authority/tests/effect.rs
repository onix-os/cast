use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationApplyReconciliation, fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::UsrRollbackFreshDbInvalidationEffectSeal,
    },
    db::state::{
        ExactFreshTransitionRemovalFault, arm_after_exact_fresh_transition_removal_attempt_before_reconciliation,
        arm_exact_fresh_transition_removal_fault, assert_exact_fresh_transition_removal_fault_consumed,
        exact_fresh_transition_removal_transaction_attempts,
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::support::{CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout};

#[test]
fn startup_fresh_db_invalidation_apply_and_finish_fix_applied_and_already_satisfied_origins() {
    for candidate_outcome in CandidateOutcome::ALL {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            candidate_outcome,
            FreshRowLayout::Present,
        );
        let canonical = fixture.canonical_bytes();
        let namespace = fixture.namespace_snapshot();
        let previous = fixture.previous_database_evidence();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_apply(&journal, &reservation);
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();

        let result = authority.reconcile(&seal, &journal).unwrap();
        let UsrRollbackFreshDbInvalidationApplyReconciliation::Applied(authority) = result else {
            panic!("reported success did not retain Applied authority");
        };
        assert_eq!(authority.origin_for_test(), RollbackActionOutcome::Applied);
        assert_eq!(fresh_db_invalidation_removal_call_count(), 1);
        assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 1);
        fixture.assert_exact_joint_absence();
        fixture.assert_journal_namespace_and_previous_unchanged(&canonical, &namespace, &previous);
    }

    for candidate_outcome in CandidateOutcome::ALL {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            candidate_outcome,
            FreshRowLayout::JointlyAbsent,
        );
        let canonical = fixture.canonical_bytes();
        let namespace = fixture.namespace_snapshot();
        let previous = fixture.previous_database_evidence();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_finish(&journal, &reservation);
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();

        let authority = authority.reconcile(&seal, &journal).unwrap();
        assert_eq!(authority.origin_for_test(), RollbackActionOutcome::AlreadySatisfied);
        assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
        fixture.assert_exact_joint_absence();
        fixture.assert_journal_namespace_and_previous_unchanged(&canonical, &namespace, &previous);
    }
}

#[test]
fn startup_fresh_db_invalidation_known_committed_error_reconciles_as_applied_once() {
    let fixture = FreshDbInvalidationFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
        FreshRowLayout::Present,
    );
    let canonical = fixture.canonical_bytes();
    let namespace = fixture.namespace_snapshot();
    let previous = fixture.previous_database_evidence();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_apply(&journal, &reservation);
    let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
    arm_exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::AfterCommit);

    let result = authority.reconcile(&seal, &journal).unwrap();
    assert_exact_fresh_transition_removal_fault_consumed();
    let UsrRollbackFreshDbInvalidationApplyReconciliation::Applied(authority) = result else {
        panic!("known committed removal did not reconcile as Applied");
    };
    assert_eq!(authority.origin_for_test(), RollbackActionOutcome::Applied);
    assert_eq!(fresh_db_invalidation_removal_call_count(), 1);
    assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 1);
    fixture.assert_exact_joint_absence();
    fixture.assert_journal_namespace_and_previous_unchanged(&canonical, &namespace, &previous);
}

#[test]
fn startup_fresh_db_invalidation_proven_nonapplication_maps_to_fieldless_not_applied_once() {
    for (fault, expected_transactions) in [
        (ExactFreshTransitionRemovalFault::BeforeTransaction, 0),
        (ExactFreshTransitionRemovalFault::BetweenProvenanceAndStateDelete, 1),
        (ExactFreshTransitionRemovalFault::BeforeCommit, 1),
    ] {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::Applied,
            FreshRowLayout::Present,
        );
        let canonical = fixture.canonical_bytes();
        let namespace = fixture.namespace_snapshot();
        let previous = fixture.previous_database_evidence();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_apply(&journal, &reservation);
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
        arm_exact_fresh_transition_removal_fault(fault);

        let result = authority.reconcile(&seal, &journal).unwrap();
        assert_exact_fresh_transition_removal_fault_consumed();
        assert!(matches!(
            result,
            UsrRollbackFreshDbInvalidationApplyReconciliation::NotApplied
        ));
        assert_eq!(fresh_db_invalidation_removal_call_count(), 1);
        assert_eq!(
            exact_fresh_transition_removal_transaction_attempts(),
            expected_transactions
        );
        fixture.assert_exact_present();
        fixture.assert_journal_namespace_and_previous_unchanged(&canonical, &namespace, &previous);
    }
}

#[test]
fn startup_fresh_db_invalidation_rollback_then_external_disappearance_stays_not_applied() {
    let fixture = FreshDbInvalidationFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
        FreshRowLayout::Present,
    );
    let canonical = fixture.canonical_bytes();
    let namespace = fixture.namespace_snapshot();
    let previous = fixture.previous_database_evidence();
    let database = fixture.fixture.fixture.database.clone();
    let candidate = fixture.fixture.fixture.candidate_state;
    let transition = fixture.record.transition_id.clone();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_apply(&journal, &reservation);
    let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
    arm_exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::BeforeCommit);
    arm_after_exact_fresh_transition_removal_attempt_before_reconciliation(move || {
        database.remove_transition_if_matches(candidate, &transition).unwrap();
    });

    let result = authority.reconcile(&seal, &journal).unwrap();
    assert_exact_fresh_transition_removal_fault_consumed();
    assert!(matches!(
        result,
        UsrRollbackFreshDbInvalidationApplyReconciliation::NotApplied
    ));
    assert_eq!(fresh_db_invalidation_removal_call_count(), 1);
    assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 1);
    fixture.assert_exact_joint_absence();
    fixture.assert_journal_namespace_and_previous_unchanged(&canonical, &namespace, &previous);
}

#[test]
fn startup_fresh_db_invalidation_uncertain_partial_changed_and_exact_aba_map_to_fieldless_ambiguous() {
    for fault in [
        ExactFreshTransitionRemovalFault::AfterCommitWithUncertainReport,
        ExactFreshTransitionRemovalFault::AfterCommitWithPartialRestoration,
        ExactFreshTransitionRemovalFault::AfterCommitWithChangedRestoration,
        ExactFreshTransitionRemovalFault::AfterCommitWithExactRestoration,
    ] {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::Exchanged,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::AlreadySatisfied,
            FreshRowLayout::Present,
        );
        let canonical = fixture.canonical_bytes();
        let namespace = fixture.namespace_snapshot();
        let previous = fixture.previous_database_evidence();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_apply(&journal, &reservation);
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
        arm_exact_fresh_transition_removal_fault(fault);

        let result = authority.reconcile(&seal, &journal).unwrap();
        assert_exact_fresh_transition_removal_fault_consumed();
        assert!(matches!(
            result,
            UsrRollbackFreshDbInvalidationApplyReconciliation::Ambiguous
        ));
        assert_eq!(fresh_db_invalidation_removal_call_count(), 1);
        assert_eq!(exact_fresh_transition_removal_transaction_attempts(), 1);
        if fault == ExactFreshTransitionRemovalFault::AfterCommitWithUncertainReport {
            fixture.assert_exact_joint_absence();
        }
        fixture.assert_journal_namespace_and_previous_unchanged(&canonical, &namespace, &previous);
    }
}

#[allow(dead_code)]
fn _pins_consuming_reconcile_signatures<'reservation>(
    apply: crate::client::startup_reconciliation::UsrRollbackFreshDbInvalidationApplyAuthority<'reservation>,
    finish: crate::client::startup_reconciliation::UsrRollbackFreshDbInvalidationFinishAuthority<'reservation>,
    seal: &UsrRollbackFreshDbInvalidationEffectSeal,
    journal: &TransitionJournalStore,
) {
    let _ = apply.reconcile(seal, journal);
    let _ = finish.reconcile(seal, journal);
}
