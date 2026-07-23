//! Phase ordering, exact-plan, topology, and operation isolation at finalization.

use std::fs;

use crate::{
    client::{startup_gate, startup_reconciliation::RecoveryBlocker},
    transition_journal::{BootRollback, ForwardPhase, Phase, RollbackAction, RollbackActionOutcome, encode},
};

use super::{
    super::{
        candidate_test_support::{CandidateLayout, CandidateSource},
        test_fixture::{OperationKind, canonical_journal},
    },
    support::{
        CandidateOrigin, Epoch, WRAPPER_INDEX, active_wrapper_path, assert_canonical_absent,
        assert_no_candidate_effects, assert_pending_phase, build_active, build_other, enter_candidate,
        enter_clean_candidate, expected_rollback_complete, persist_candidate_preserved, persist_rollback_complete,
        reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_finalization_keeps_candidate_preserved_and_terminal_deletion_on_separate_entries() {
    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let preserved = persist_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let complete = expected_rollback_complete(&preserved);
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let first = enter_candidate(&fixture);

    assert_pending_phase(&first, Phase::RollbackComplete);
    assert_eq!(fixture.fixture.canonical_record(), complete);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    let clean = enter_clean_candidate(&fixture);

    assert_canonical_absent(&fixture.fixture.installation.root);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
    drop(clean);
}

#[test]
fn startup_active_reblit_finalization_rejects_a_valid_terminal_lookalike_plan_and_wrong_topology() {
    let lookalike = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&lookalike, CandidateOrigin::AlreadySatisfied);
    let mut inexact = source.clone();
    let rollback = inexact.rollback.as_mut().unwrap();
    rollback.source = ForwardPhase::TransactionTriggersComplete;
    rollback.usr_exchange = RollbackAction::NotRequired;
    assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
    assert_eq!(rollback.fresh_db, RollbackAction::NotRequired);
    assert_eq!(rollback.boot, BootRollback::NotRequired);
    rollback.external_effects_may_remain = true;
    let terminal_lookalike = inexact.rollback_successor(None).unwrap();
    assert_eq!(terminal_lookalike.phase, Phase::RollbackComplete);
    fs::write(
        canonical_journal(&lookalike.fixture.installation.root),
        encode(&terminal_lookalike).unwrap(),
    )
    .unwrap();
    let database_before = lookalike.fixture.database_snapshot();
    let namespace_before = lookalike.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let plan_error = enter_candidate(&lookalike);

    assert_pending_phase(&plan_error, Phase::RollbackComplete);
    assert_eq!(lookalike.fixture.canonical_record(), terminal_lookalike);
    assert_eq!(lookalike.fixture.database_snapshot(), database_before);
    assert_eq!(lookalike.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    let topology = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&topology, CandidateOrigin::Applied);
    let exact_wrapper = active_wrapper_path(&topology);
    let displaced = topology.fixture.installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-{}-{}-{}",
        i32::from(topology.fixture.previous_state) + 1,
        terminal.previous.tree_token.as_str(),
        WRAPPER_INDEX
    ));
    fs::rename(&exact_wrapper, &displaced).unwrap();
    let database_before = topology.fixture.database_snapshot();
    let namespace_before = topology.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let topology_error = enter_candidate(&topology);

    assert_pending_phase(&topology_error, Phase::RollbackComplete);
    assert_pending_any_blocker(
        &topology_error,
        &[
            RecoveryBlocker::ActivationNamespaceRejected,
            RecoveryBlocker::PhaseNamespaceConflict,
        ],
    );
    assert_eq!(topology.fixture.canonical_record(), terminal);
    assert_eq!(topology.fixture.database_snapshot(), database_before);
    assert_eq!(topology.fixture.namespace_snapshot(), namespace_before);
    assert!(!exact_wrapper.exists());
    assert!(displaced.join("usr").is_dir());
    assert_no_candidate_effects();
}

#[test]
fn startup_active_reblit_finalization_keeps_new_state_and_archived_in_their_own_routes() {
    let archived = build_other(
        OperationKind::Archived,
        CandidateSource::Exchanged,
        CandidateLayout::Preserved,
    );
    let archived_preserved = persist_candidate_preserved(&archived, CandidateOrigin::Applied);
    let archived_terminal = archived_preserved.rollback_successor(None).unwrap();
    let journal = archived.open_journal();
    journal.advance(&archived_preserved, &archived_terminal).unwrap();
    drop(journal);
    let archived_database = archived.fixture.database_snapshot();
    let archived_namespace = archived.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let archived_clean = enter_clean_candidate(&archived);

    assert_canonical_absent(&archived.fixture.installation.root);
    assert_eq!(archived.fixture.database_snapshot(), archived_database);
    assert_eq!(archived.fixture.namespace_snapshot(), archived_namespace);
    assert_no_candidate_effects();
    drop(archived_clean);

    let new_state = build_other(
        OperationKind::NewState,
        CandidateSource::Intent,
        CandidateLayout::Preserved,
    );
    let new_state_preserved = persist_candidate_preserved(&new_state, CandidateOrigin::AlreadySatisfied);
    let expected = new_state_preserved.rollback_successor(None).unwrap();
    let database_before = new_state.fixture.database_snapshot();
    let namespace_before = new_state.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let new_state_error = enter_candidate(&new_state);

    assert_pending_phase(&new_state_error, Phase::FreshDbInvalidationIntent);
    assert_eq!(new_state.fixture.canonical_record(), expected);
    assert_eq!(new_state.fixture.database_snapshot(), database_before);
    assert_eq!(new_state.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
}

fn assert_pending_any_blocker(error: &startup_gate::Error, expected: &[RecoveryBlocker]) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected recovery-pending topology refusal, got {error:?}");
    };
    assert!(
        expected.iter().any(|blocker| pending.blockers().contains(blocker)),
        "expected one of {expected:?}, got {:?}",
        pending.blockers()
    );
}
