//! Focused contracts for sealed reverse-effect reconciliation.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackReverseAdmission, UsrRollbackReverseApplyReconciliation,
            arm_before_reverse_exchange_reconciliation_capture,
            arm_before_usr_rollback_reverse_effect_final_namespace_capture,
        },
        startup_recovery::UsrRollbackReverseEffectSeal,
    },
    transition_identity::{
        RetainedExchangeSyscallFault, arm_retained_exchange_syscall_fault, reset_retained_exchange_syscall_count,
        retained_exchange_syscall_count,
    },
    transition_journal::RollbackActionOutcome,
};

use super::super::test_support::{EffectOperationKind, ReverseFixture, ReverseLayout};

#[test]
fn startup_usr_rollback_reverse_apply_reconciles_every_raw_result_for_every_operation() {
    let cases = [
        (None, true),
        (Some(RetainedExchangeSyscallFault::ErrorAfterApply), true),
        (Some(RetainedExchangeSyscallFault::ErrorWithoutApply), false),
        (Some(RetainedExchangeSyscallFault::SuccessWithoutApply), false),
    ];

    for kind in EffectOperationKind::ALL {
        for (fault, expected_applied) in cases {
            let fixture = ReverseFixture::for_effect(kind, ReverseLayout::Post);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let seal = UsrRollbackReverseEffectSeal::new_for_test();
            let UsrRollbackReverseAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
                panic!("POST {kind:?} evidence did not admit apply authority");
            };
            let lease = authority.into_effect_lease(&seal, &journal).unwrap();
            match fault {
                Some(fault) => arm_retained_exchange_syscall_fault(fault),
                None => reset_retained_exchange_syscall_count(),
            }

            let reconciliation = lease.reconcile(&seal, &journal).unwrap();

            assert_eq!(retained_exchange_syscall_count(), 1, "{kind:?} {fault:?}");
            match (expected_applied, reconciliation) {
                (true, UsrRollbackReverseApplyReconciliation::Applied(authority)) => {
                    assert_eq!(authority.outcome_for_test(), RollbackActionOutcome::Applied);
                    assert!(matches!(
                        fixture.capture(&journal, &reservation),
                        UsrRollbackReverseAdmission::Finish(_)
                    ));
                }
                (false, UsrRollbackReverseApplyReconciliation::NotApplied) => {
                    assert!(matches!(
                        fixture.capture(&journal, &reservation),
                        UsrRollbackReverseAdmission::Apply(_)
                    ));
                }
                (_, UsrRollbackReverseApplyReconciliation::Ambiguous) => {
                    panic!("stable {kind:?} {fault:?} evidence reconciled as ambiguous")
                }
                (true, UsrRollbackReverseApplyReconciliation::NotApplied) => {
                    panic!("applied {kind:?} {fault:?} attempt reconciled as not applied")
                }
                (false, UsrRollbackReverseApplyReconciliation::Applied(_)) => {
                    panic!("unapplied {kind:?} {fault:?} attempt reconciled as applied")
                }
            }
            fixture.assert_non_namespace_unchanged();
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_apply_ambiguity_consumes_all_retry_capability() {
    for kind in EffectOperationKind::ALL {
        let fixture = ReverseFixture::for_effect(kind, ReverseLayout::Post);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let seal = UsrRollbackReverseEffectSeal::new_for_test();
        let UsrRollbackReverseAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
            panic!("POST {kind:?} evidence did not admit apply authority");
        };
        let lease = authority.into_effect_lease(&seal, &journal).unwrap();
        arm_before_reverse_exchange_reconciliation_capture(
            fixture.namespace_change_hook(format!("rollback-reverse-post-attempt-ambiguity-{kind:?}")),
        );
        reset_retained_exchange_syscall_count();

        assert!(matches!(
            lease.reconcile(&seal, &journal).unwrap(),
            UsrRollbackReverseApplyReconciliation::Ambiguous
        ));
        assert_eq!(retained_exchange_syscall_count(), 1);
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_usr_rollback_reverse_apply_final_post_race_prevents_the_attempt() {
    let fixture = ReverseFixture::for_effect(EffectOperationKind::Archived, ReverseLayout::Post);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackReverseEffectSeal::new_for_test();
    let UsrRollbackReverseAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("POST evidence did not admit apply authority");
    };
    let lease = authority.into_effect_lease(&seal, &journal).unwrap();
    arm_before_usr_rollback_reverse_effect_final_namespace_capture(
        fixture.namespace_change_hook("rollback-reverse-final-post-race".to_owned()),
    );
    reset_retained_exchange_syscall_count();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(retained_exchange_syscall_count(), 0);
}

#[test]
fn startup_usr_rollback_reverse_finish_is_zero_call_for_every_operation() {
    for kind in EffectOperationKind::ALL {
        let fixture = ReverseFixture::for_effect(kind, ReverseLayout::Pre);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let seal = UsrRollbackReverseEffectSeal::new_for_test();
        let UsrRollbackReverseAdmission::Finish(authority) = fixture.capture(&journal, &reservation) else {
            panic!("PRE {kind:?} evidence did not admit finish authority");
        };
        let lease = authority.into_effect_lease(&seal, &journal).unwrap();
        reset_retained_exchange_syscall_count();

        let authority = lease.reconcile(&seal, &journal).unwrap();

        assert_eq!(retained_exchange_syscall_count(), 0, "{kind:?}");
        assert_eq!(authority.outcome_for_test(), RollbackActionOutcome::AlreadySatisfied);
        fixture.assert_non_namespace_unchanged();
        assert!(matches!(
            fixture.capture(&journal, &reservation),
            UsrRollbackReverseAdmission::Finish(_)
        ));
    }
}

#[test]
fn startup_usr_rollback_reverse_effect_consumption_starts_with_the_open_binding() {
    let fixture = ReverseFixture::for_effect(EffectOperationKind::ActiveReblit, ReverseLayout::Post);
    let first = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackReverseEffectSeal::new_for_test();
    let UsrRollbackReverseAdmission::Apply(authority) = fixture.capture(&first, &reservation) else {
        panic!("POST evidence did not admit apply authority");
    };
    let lease = authority.into_effect_lease(&seal, &first).unwrap();
    drop(first);
    let second = fixture.open_journal();
    reset_retained_exchange_syscall_count();

    assert!(lease.reconcile(&seal, &second).is_err());
    assert_eq!(retained_exchange_syscall_count(), 0);
}

#[test]
fn startup_usr_rollback_reverse_apply_rechecks_database_after_namespace_use() {
    let fixture = ReverseFixture::for_effect(EffectOperationKind::NewState, ReverseLayout::Post);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackReverseEffectSeal::new_for_test();
    let UsrRollbackReverseAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("POST evidence did not admit apply authority");
    };
    let lease = authority.into_effect_lease(&seal, &journal).unwrap();
    arm_before_reverse_exchange_reconciliation_capture(fixture.candidate_transition_clear_hook());
    reset_retained_exchange_syscall_count();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(retained_exchange_syscall_count(), 1);
}
