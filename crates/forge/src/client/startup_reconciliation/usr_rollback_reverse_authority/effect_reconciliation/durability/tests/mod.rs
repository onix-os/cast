//! Focused contracts for the sealed reverse parent-durability bridge.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackReverseAdmission, UsrRollbackReverseAlreadySatisfiedEffectAuthority,
            UsrRollbackReverseAppliedEffectAuthority, UsrRollbackReverseApplyReconciliation,
            activation_namespace::{
                UsrRollbackReverseNamespaceDurabilityEvent, UsrRollbackReverseNamespaceDurabilityFaultPoint,
                arm_before_usr_rollback_reverse_namespace_final_pre_capture,
                arm_before_usr_rollback_reverse_namespace_installation_root_sync,
                arm_usr_rollback_reverse_namespace_durability_fault,
                reset_usr_rollback_reverse_namespace_durability_events,
                take_usr_rollback_reverse_namespace_durability_events,
            },
        },
        startup_recovery::{
            UsrRollbackReverseEffectSeal, complete_already_satisfied_usr_rollback_reverse_durability,
            complete_applied_usr_rollback_reverse_durability,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::super::super::test_support::{EffectOperationKind, ReverseFixture, ReverseLayout};

fn reconcile_applied<'reservation>(
    fixture: &ReverseFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackReverseAppliedEffectAuthority<'reservation> {
    let seal = UsrRollbackReverseEffectSeal::new_for_test();
    let UsrRollbackReverseAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
        panic!("exact POST evidence did not admit apply authority");
    };
    let lease = authority.into_effect_lease(&seal, journal).unwrap();
    let UsrRollbackReverseApplyReconciliation::Applied(authority) = lease.reconcile(&seal, journal).unwrap() else {
        panic!("normal reverse exchange did not reconcile as applied");
    };
    authority
}

fn reconcile_already_satisfied<'reservation>(
    fixture: &ReverseFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackReverseAlreadySatisfiedEffectAuthority<'reservation> {
    let seal = UsrRollbackReverseEffectSeal::new_for_test();
    let UsrRollbackReverseAdmission::Finish(authority) = fixture.capture(journal, reservation) else {
        panic!("exact PRE evidence did not admit finish authority");
    };
    authority
        .into_effect_lease(&seal, journal)
        .unwrap()
        .reconcile(&seal, journal)
        .unwrap()
}

fn reset_events() {
    reset_usr_rollback_reverse_namespace_durability_events();
    assert!(take_usr_rollback_reverse_namespace_durability_events().is_empty());
}

fn take_events() -> Vec<UsrRollbackReverseNamespaceDurabilityEvent> {
    take_usr_rollback_reverse_namespace_durability_events()
}

fn success_events(fixture: &ReverseFixture) -> Vec<UsrRollbackReverseNamespaceDurabilityEvent> {
    let ((staging_device, staging_inode), (root_device, root_inode)) = fixture.durability_parent_identities();
    vec![
        UsrRollbackReverseNamespaceDurabilityEvent::StagingParentSynced {
            device: staging_device,
            inode: staging_inode,
        },
        UsrRollbackReverseNamespaceDurabilityEvent::InstallationRootSynced {
            device: root_device,
            inode: root_inode,
        },
        UsrRollbackReverseNamespaceDurabilityEvent::FinalPreProven,
    ]
}

#[derive(Clone, Copy, Debug)]
enum DurabilityRace {
    Database,
    Journal,
    Namespace,
}

#[derive(Clone, Copy, Debug)]
enum DurabilityRaceBoundary {
    BeforeSync,
    BetweenSyncs,
    AfterParentSyncs,
}

fn exercise_durability_race(boundary: DurabilityRaceBoundary, race: DurabilityRace, initial_layout: ReverseLayout) {
    let operation = match race {
        DurabilityRace::Database => EffectOperationKind::NewState,
        DurabilityRace::Journal | DurabilityRace::Namespace => EffectOperationKind::Archived,
    };
    let fixture = ReverseFixture::for_effect(operation, initial_layout);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_retained_exchange_syscall_count();

    match initial_layout {
        ReverseLayout::Post => {
            let authority = reconcile_applied(&fixture, &journal, &reservation);
            arm_durability_race(&fixture, boundary, race);
            assert!(complete_applied_usr_rollback_reverse_durability(&journal, authority).is_err());
        }
        ReverseLayout::Pre => {
            let authority = reconcile_already_satisfied(&fixture, &journal, &reservation);
            arm_durability_race(&fixture, boundary, race);
            assert!(complete_already_satisfied_usr_rollback_reverse_durability(&journal, authority).is_err());
        }
    }

    assert_eq!(
        retained_exchange_syscall_count(),
        usize::from(initial_layout == ReverseLayout::Post),
        "{boundary:?} {race:?} {initial_layout:?}"
    );
    assert_eq!(
        take_events(),
        expected_race_events(&fixture, boundary, race),
        "{boundary:?} {race:?} {initial_layout:?}"
    );
}

fn arm_durability_race(fixture: &ReverseFixture, boundary: DurabilityRaceBoundary, race: DurabilityRace) {
    let hook: Box<dyn FnOnce()> = match race {
        DurabilityRace::Database => Box::new(fixture.candidate_transition_clear_hook()),
        DurabilityRace::Journal => Box::new(fixture.journal_change_hook()),
        DurabilityRace::Namespace => {
            Box::new(fixture.namespace_change_hook(format!("reverse-durability-{boundary:?}-{race:?}")))
        }
    };
    reset_events();
    match boundary {
        DurabilityRaceBoundary::BeforeSync => hook(),
        DurabilityRaceBoundary::BetweenSyncs => arm_before_usr_rollback_reverse_namespace_installation_root_sync(hook),
        DurabilityRaceBoundary::AfterParentSyncs => arm_before_usr_rollback_reverse_namespace_final_pre_capture(hook),
    }
}

fn expected_race_events(
    fixture: &ReverseFixture,
    boundary: DurabilityRaceBoundary,
    race: DurabilityRace,
) -> Vec<UsrRollbackReverseNamespaceDurabilityEvent> {
    let completed = match (boundary, race) {
        (DurabilityRaceBoundary::BeforeSync, _) => 0,
        (DurabilityRaceBoundary::BetweenSyncs, DurabilityRace::Namespace) => 1,
        (DurabilityRaceBoundary::AfterParentSyncs, DurabilityRace::Namespace) => 2,
        (DurabilityRaceBoundary::BetweenSyncs | DurabilityRaceBoundary::AfterParentSyncs, _) => 3,
    };
    success_events(fixture)[..completed].to_vec()
}

fn exercise_durability_race_matrix(boundary: DurabilityRaceBoundary) {
    for race in [
        DurabilityRace::Database,
        DurabilityRace::Journal,
        DurabilityRace::Namespace,
    ] {
        for initial_layout in [ReverseLayout::Post, ReverseLayout::Pre] {
            exercise_durability_race(boundary, race, initial_layout);
        }
    }
}

#[test]
fn reverse_durability_constructs_outcome_only_after_both_parent_barriers_for_every_operation() {
    for kind in EffectOperationKind::ALL {
        let fixture = ReverseFixture::for_effect(kind, ReverseLayout::Post);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_retained_exchange_syscall_count();
        let authority = reconcile_applied(&fixture, &journal, &reservation);
        assert_eq!(retained_exchange_syscall_count(), 1, "{kind:?}");
        reset_events();

        let durable = complete_applied_usr_rollback_reverse_durability(&journal, authority).unwrap();

        assert_eq!(durable.outcome_for_test(), RollbackActionOutcome::Applied, "{kind:?}");
        assert_eq!(take_events(), success_events(&fixture), "{kind:?}");
        assert_eq!(retained_exchange_syscall_count(), 1, "{kind:?}");
        fixture.assert_non_namespace_unchanged();
        drop(durable);
        drop(reservation);
        drop(journal);

        let fixture = ReverseFixture::for_effect(kind, ReverseLayout::Pre);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_retained_exchange_syscall_count();
        let authority = reconcile_already_satisfied(&fixture, &journal, &reservation);
        reset_events();

        let durable = complete_already_satisfied_usr_rollback_reverse_durability(&journal, authority).unwrap();

        assert_eq!(
            durable.outcome_for_test(),
            RollbackActionOutcome::AlreadySatisfied,
            "{kind:?}"
        );
        assert_eq!(take_events(), success_events(&fixture), "{kind:?}");
        assert_eq!(retained_exchange_syscall_count(), 0, "{kind:?}");
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn reverse_durability_binding_is_the_first_check_for_both_provenances() {
    let fixture = ReverseFixture::for_effect(EffectOperationKind::Archived, ReverseLayout::Post);
    let first = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = reconcile_applied(&fixture, &first, &reservation);
    drop(first);
    let second = fixture.open_journal();
    reset_events();

    assert!(complete_applied_usr_rollback_reverse_durability(&second, authority).is_err());
    assert!(take_events().is_empty());
    drop(second);
    drop(reservation);

    let fixture = ReverseFixture::for_effect(EffectOperationKind::Archived, ReverseLayout::Pre);
    let first = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = reconcile_already_satisfied(&fixture, &first, &reservation);
    drop(first);
    let second = fixture.open_journal();
    reset_events();

    assert!(complete_already_satisfied_usr_rollback_reverse_durability(&second, authority).is_err());
    assert!(take_events().is_empty());
}

#[test]
fn reverse_durability_faults_consume_authority_at_each_ordered_boundary() {
    for (point, completed_events) in [
        (UsrRollbackReverseNamespaceDurabilityFaultPoint::StagingParentSync, 0),
        (UsrRollbackReverseNamespaceDurabilityFaultPoint::InstallationRootSync, 1),
        (UsrRollbackReverseNamespaceDurabilityFaultPoint::FinalPreCapture, 2),
    ] {
        for initial_layout in [ReverseLayout::Post, ReverseLayout::Pre] {
            let fixture = ReverseFixture::for_effect(EffectOperationKind::ActiveReblit, initial_layout);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_retained_exchange_syscall_count();
            let expected = success_events(&fixture)[..completed_events].to_vec();
            reset_events();
            arm_usr_rollback_reverse_namespace_durability_fault(point);

            match initial_layout {
                ReverseLayout::Post => {
                    let authority = reconcile_applied(&fixture, &journal, &reservation);
                    assert!(complete_applied_usr_rollback_reverse_durability(&journal, authority).is_err());
                    assert_eq!(retained_exchange_syscall_count(), 1, "{point:?} {initial_layout:?}");
                }
                ReverseLayout::Pre => {
                    let authority = reconcile_already_satisfied(&fixture, &journal, &reservation);
                    assert!(complete_already_satisfied_usr_rollback_reverse_durability(&journal, authority).is_err());
                    assert_eq!(retained_exchange_syscall_count(), 0, "{point:?} {initial_layout:?}");
                }
            }

            assert_eq!(take_events(), expected, "{point:?} {initial_layout:?}");
            fixture.assert_non_namespace_unchanged();
            drop(reservation);
            drop(journal);

            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = reconcile_already_satisfied(&fixture, &journal, &reservation);
            reset_events();
            let durable = complete_already_satisfied_usr_rollback_reverse_durability(&journal, authority).unwrap();

            assert_eq!(durable.outcome_for_test(), RollbackActionOutcome::AlreadySatisfied);
            assert_eq!(
                take_events(),
                success_events(&fixture),
                "restart {point:?} {initial_layout:?}"
            );
            assert_eq!(
                retained_exchange_syscall_count(),
                usize::from(initial_layout == ReverseLayout::Post),
                "restart {point:?} {initial_layout:?}"
            );
            fixture.assert_non_namespace_unchanged();
        }
    }
}

#[test]
fn reverse_durability_rejects_database_journal_and_namespace_changes_before_sync() {
    exercise_durability_race_matrix(DurabilityRaceBoundary::BeforeSync);
}

#[test]
fn reverse_durability_rejects_database_journal_and_namespace_changes_between_syncs() {
    exercise_durability_race_matrix(DurabilityRaceBoundary::BetweenSyncs);
}

#[test]
fn reverse_durability_rejects_database_journal_and_namespace_changes_after_parent_syncs() {
    exercise_durability_race_matrix(DurabilityRaceBoundary::AfterParentSyncs);
}
