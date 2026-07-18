//! Strict operation, phase, source, and complete-plan field boundaries.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{UsrRollbackActiveReblitBootRepairRequiredSeal, UsrRollbackActiveReblitCompleteRouteSeal},
        startup_reconciliation::{
            UsrRollbackActiveReblitBootRepairRequiredAdmission, UsrRollbackActiveReblitBootRepairRequiredAuthority,
            UsrRollbackActiveReblitCompleteRouteAdmission, UsrRollbackActiveReblitCompleteRouteAuthority,
        },
    },
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::{
    super::test_fixture::BootSyncStartedLayout,
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, assert_no_boot_synchronize_attempts, assert_no_candidate_effects,
        assert_pending_phase, build_boot_sync_started, drive_boot_sync_started_to_candidate_preserved, enter_boot,
        reset_boot_synchronize_observer, reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_boot_repair_required_rejects_each_inexact_plan_field() {
    let fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
    reset_boot_synchronize_observer();
    let exact =
        drive_boot_sync_started_to_candidate_preserved(&fixture, UsrRestoreOrigin::Applied, CandidateOrigin::Applied);
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    let journal = open_journal(&fixture.fixture.installation);
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_candidate_effect_observers();

    assert_boot_ready(&fixture.fixture, &journal, &reservation, &exact);
    assert_completion_refused(&fixture.fixture, &journal, &reservation, &exact);

    for source in [
        ForwardPhase::BootSyncComplete,
        ForwardPhase::TransactionTriggersComplete,
        ForwardPhase::SystemTriggersComplete,
    ] {
        let mut changed = exact.clone();
        changed.rollback.as_mut().unwrap().source = source;
        assert_boot_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    for source in [ForwardPhase::UsrExchangeIntent, ForwardPhase::UsrExchanged] {
        let mut legacy = exact.clone();
        let rollback = legacy.rollback.as_mut().unwrap();
        rollback.source = source;
        rollback.boot = BootRollback::NotRequired;
        assert_boot_refused(&fixture.fixture, &journal, &reservation, &legacy);
    }

    for boot in [BootRollback::NotRequired, BootRollback::Unverified] {
        let mut changed = exact.clone();
        changed.rollback.as_mut().unwrap().boot = boot;
        assert_boot_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    for phase in [
        Phase::BootSyncStarted,
        Phase::RollbackDecided,
        Phase::CandidatePreserveIntent,
        Phase::BootRepairRequired,
        Phase::BootRepairStarted,
        Phase::BootRepairUnverified,
        Phase::RollbackComplete,
    ] {
        let mut changed = exact.clone();
        changed.phase = phase;
        assert_boot_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    for operation in [Operation::NewState, Operation::ActivateArchived] {
        let mut changed = exact.clone();
        changed.operation = operation;
        assert_boot_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    let mut external_effects_cleared = exact.clone();
    external_effects_cleared
        .rollback
        .as_mut()
        .unwrap()
        .external_effects_may_remain = false;
    assert_boot_refused(&fixture.fixture, &journal, &reservation, &external_effects_cleared);

    for action in [RollbackAction::Pending, RollbackAction::NotRequired] {
        let mut changed = exact.clone();
        changed.rollback.as_mut().unwrap().usr_exchange = action;
        assert_boot_refused(&fixture.fixture, &journal, &reservation, &changed);

        let mut changed = exact.clone();
        changed.rollback.as_mut().unwrap().candidate.action = action;
        assert_boot_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    for action in [RollbackAction::Pending, RollbackAction::Applied] {
        let mut changed = exact.clone();
        changed.rollback.as_mut().unwrap().fresh_db = action;
        assert_boot_refused(&fixture.fixture, &journal, &reservation, &changed);

        let mut changed = exact.clone();
        changed.rollback.as_mut().unwrap().previous_archive = action;
        assert_boot_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    let mut wrong_disposition = exact.clone();
    wrong_disposition.rollback.as_mut().unwrap().candidate.disposition = AbortDisposition::Rearchive;
    assert_boot_refused(&fixture.fixture, &journal, &reservation, &wrong_disposition);

    let mut missing_candidate = exact.clone();
    missing_candidate.candidate.id = None;
    assert_boot_refused(&fixture.fixture, &journal, &reservation, &missing_candidate);

    let mut mismatched_candidate = exact.clone();
    mismatched_candidate.candidate.id = exact.previous.id.map(|id| id + 1);
    assert_boot_refused(&fixture.fixture, &journal, &reservation, &mismatched_candidate);

    assert_eq!(fixture.fixture.canonical_record(), exact);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();
}

#[test]
fn startup_active_reblit_boot_repair_required_physical_pre_layout_defers_without_mutation() {
    for epoch in Epoch::ALL {
        let fixture = build_boot_sync_started(epoch, BootSyncStartedLayout::Pre);
        let source = fixture.fixture.source.clone();
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        reset_candidate_effect_observers();
        reset_boot_synchronize_observer();

        let error = enter_boot(&fixture);

        assert_pending_phase(&error, Phase::BootSyncStarted);
        assert_eq!(fixture.fixture.canonical_record(), source);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_no_candidate_effects();
        assert_no_boot_synchronize_attempts();
    }
}

fn assert_boot_ready(
    fixture: &super::super::test_fixture::Fixture,
    journal: &TransitionJournalStore,
    reservation: &ActiveStateReservation,
    record: &TransitionRecord,
) {
    let seal = UsrRollbackActiveReblitBootRepairRequiredSeal::new_for_test();
    let admission = UsrRollbackActiveReblitBootRepairRequiredAuthority::capture(
        &seal,
        &fixture.installation,
        journal,
        &fixture.database,
        reservation,
        record,
    )
    .unwrap();
    assert!(matches!(
        admission,
        UsrRollbackActiveReblitBootRepairRequiredAdmission::Ready(_)
    ));
}

fn assert_boot_refused(
    fixture: &super::super::test_fixture::Fixture,
    journal: &TransitionJournalStore,
    reservation: &ActiveStateReservation,
    record: &TransitionRecord,
) {
    let seal = UsrRollbackActiveReblitBootRepairRequiredSeal::new_for_test();
    let admission = UsrRollbackActiveReblitBootRepairRequiredAuthority::capture(
        &seal,
        &fixture.installation,
        journal,
        &fixture.database,
        reservation,
        record,
    )
    .unwrap();
    assert!(!matches!(
        admission,
        UsrRollbackActiveReblitBootRepairRequiredAdmission::Ready(_)
    ));
}

fn assert_completion_refused(
    fixture: &super::super::test_fixture::Fixture,
    journal: &TransitionJournalStore,
    reservation: &ActiveStateReservation,
    record: &TransitionRecord,
) {
    let seal = UsrRollbackActiveReblitCompleteRouteSeal::new_for_test();
    let admission = UsrRollbackActiveReblitCompleteRouteAuthority::capture(
        &seal,
        &fixture.installation,
        journal,
        &fixture.database,
        reservation,
        record,
    )
    .unwrap();
    assert!(!matches!(
        admission,
        UsrRollbackActiveReblitCompleteRouteAdmission::Ready(_)
    ));
}

fn open_journal(installation: &crate::Installation) -> TransitionJournalStore {
    TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap()
}
