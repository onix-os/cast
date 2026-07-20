//! Strict operation, phase, plan, journal, and topology boundaries.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActiveReblitBootRepairCompleteSeal,
        startup_reconciliation::{
            UsrRollbackActiveReblitBootRepairCompleteAdmission,
            UsrRollbackActiveReblitBootRepairCompleteAuthority,
        },
    },
    transition_journal::{
        AbortDisposition, BootRepairOutcome, BootRollback, ForwardPhase, Operation, Phase, RollbackAction,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::{
    super::test_fixture::BootSyncStartedLayout,
    support::{
        CandidateOrigin, Epoch, UsrRestoreOrigin, WRAPPER_INDEX, assert_no_boot_synchronize_attempts,
        assert_no_candidate_effects, assert_pending_phase, build_boot_sync_started,
        capture_boot_repair_complete_ready, drive_boot_sync_started_to_candidate_preserved, enter_boot,
        expected_boot_repair_required, reset_boot_synchronize_observer, reset_candidate_effect_observers,
        seed_boot_repair_complete_for_test,
    },
};

#[test]
fn startup_active_reblit_boot_repair_complete_rejects_every_inexact_route_shape() {
    let fixture = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
    let preserved = drive_boot_sync_started_to_candidate_preserved(
        &fixture,
        UsrRestoreOrigin::Applied,
        CandidateOrigin::Applied,
    );
    let required = expected_boot_repair_required(&preserved);
    let required_entry = enter_boot(&fixture);
    assert_pending_phase(&required_entry, Phase::BootRepairRequired);
    let started = super::support::seed_boot_repair_started_for_test(&fixture, &required);
    let complete = started
        .boot_repair_complete_successor(BootRepairOutcome::Applied)
        .unwrap();
    let journal = open_journal(&fixture.fixture.installation);
    journal.advance(&started, &complete).unwrap();
    drop(journal);
    let journal = open_journal(&fixture.fixture.installation);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_boot_synchronize_observer();
    reset_candidate_effect_observers();

    let authority = capture_boot_repair_complete_ready(&fixture, &journal, &reservation, &complete);
    assert_eq!(authority.wrapper_index(), WRAPPER_INDEX);
    drop(authority);

    assert_complete_refused(&fixture.fixture, &journal, &reservation, &started);

    for phase in [
        Phase::CandidatePreserved,
        Phase::BootRepairRequired,
        Phase::BootRepairStarted,
        Phase::BootRepairUnverified,
        Phase::RollbackComplete,
    ] {
        let mut changed = complete.clone();
        changed.phase = phase;
        assert_complete_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    for operation in [Operation::NewState, Operation::ActivateArchived] {
        let mut changed = complete.clone();
        changed.operation = operation;
        assert_complete_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    for source in [
        ForwardPhase::UsrExchangeIntent,
        ForwardPhase::UsrExchanged,
        ForwardPhase::BootSyncComplete,
        ForwardPhase::TransactionTriggersComplete,
    ] {
        let mut changed = complete.clone();
        changed.rollback.as_mut().unwrap().source = source;
        assert_complete_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    for boot in [
        BootRollback::NotRequired,
        BootRollback::PendingUnverifiable,
        BootRollback::Unverified,
    ] {
        let mut changed = complete.clone();
        changed.rollback.as_mut().unwrap().boot = boot;
        assert_complete_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    for action in [RollbackAction::Pending, RollbackAction::NotRequired] {
        let mut changed = complete.clone();
        changed.rollback.as_mut().unwrap().usr_exchange = action;
        assert_complete_refused(&fixture.fixture, &journal, &reservation, &changed);

        let mut changed = complete.clone();
        changed.rollback.as_mut().unwrap().candidate.action = action;
        assert_complete_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    for action in [RollbackAction::Pending, RollbackAction::Applied] {
        let mut changed = complete.clone();
        changed.rollback.as_mut().unwrap().previous_archive = action;
        assert_complete_refused(&fixture.fixture, &journal, &reservation, &changed);

        let mut changed = complete.clone();
        changed.rollback.as_mut().unwrap().fresh_db = action;
        assert_complete_refused(&fixture.fixture, &journal, &reservation, &changed);
    }

    let mut wrong_disposition = complete.clone();
    wrong_disposition.rollback.as_mut().unwrap().candidate.disposition = AbortDisposition::Rearchive;
    assert_complete_refused(&fixture.fixture, &journal, &reservation, &wrong_disposition);

    let mut missing_candidate = complete.clone();
    missing_candidate.candidate.id = None;
    assert_complete_refused(&fixture.fixture, &journal, &reservation, &missing_candidate);

    let mut mismatched_candidate = complete.clone();
    mismatched_candidate.candidate.id = complete.previous.id.map(|id| id + 1);
    assert_complete_refused(&fixture.fixture, &journal, &reservation, &mismatched_candidate);

    let mut effects_cleared = complete.clone();
    effects_cleared.rollback.as_mut().unwrap().external_effects_may_remain = false;
    assert_complete_refused(&fixture.fixture, &journal, &reservation, &effects_cleared);

    assert_eq!(fixture.fixture.canonical_record(), complete);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
    assert_no_boot_synchronize_attempts();
}

#[test]
fn startup_active_reblit_boot_repair_complete_authority_is_bound_to_its_open_journal() {
    let fixture = build_boot_sync_started(Epoch::Historical, BootSyncStartedLayout::Post);
    let preserved = drive_boot_sync_started_to_candidate_preserved(
        &fixture,
        UsrRestoreOrigin::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let required = expected_boot_repair_required(&preserved);
    let required_entry = enter_boot(&fixture);
    assert_pending_phase(&required_entry, Phase::BootRepairRequired);
    let complete = seed_boot_repair_complete_for_test(&fixture, &required, BootRepairOutcome::AlreadySatisfied);
    let journal = open_journal(&fixture.fixture.installation);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = capture_boot_repair_complete_ready(&fixture, &journal, &reservation, &complete);

    let other = build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post);
    let other_journal = open_journal(&other.fixture.installation);

    assert!(authority.revalidate(&other_journal).is_err());
    assert_eq!(fixture.fixture.canonical_record(), complete);
    assert_no_boot_synchronize_attempts();
}

fn assert_complete_refused(
    fixture: &super::super::test_fixture::Fixture,
    journal: &TransitionJournalStore,
    reservation: &ActiveStateReservation,
    record: &TransitionRecord,
) {
    let seal = UsrRollbackActiveReblitBootRepairCompleteSeal::new_for_test();
    let admission = UsrRollbackActiveReblitBootRepairCompleteAuthority::capture(
        &seal,
        &fixture.installation,
        &fixture.database,
        journal,
        reservation,
        record,
    )
    .unwrap();
    assert!(!matches!(
        admission,
        UsrRollbackActiveReblitBootRepairCompleteAdmission::Ready(_)
    ));
}

fn open_journal(installation: &crate::Installation) -> TransitionJournalStore {
    TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap()
}
