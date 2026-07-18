//! Operation, phase, plan, and topology boundaries for completion routing.

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
        enter_clean_candidate, expected_rollback_complete, persist_candidate_preserved,
        reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_complete_route_preserves_operation_and_phase_ordering() {
    let archived = build_other(
        OperationKind::Archived,
        CandidateSource::Exchanged,
        CandidateLayout::Preserved,
    );
    let archived_preserved = persist_candidate_preserved(&archived, CandidateOrigin::Applied);
    let archived_database = archived.fixture.database_snapshot();
    let archived_namespace = archived.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let archived_error = enter_candidate(&archived);

    assert_pending_phase(&archived_error, Phase::CandidatePreserved);
    assert_eq!(archived.fixture.canonical_record(), archived_preserved);
    assert_eq!(archived.fixture.database_snapshot(), archived_database);
    assert_eq!(archived.fixture.namespace_snapshot(), archived_namespace);
    assert_no_candidate_effects();

    let new_state = build_other(
        OperationKind::NewState,
        CandidateSource::Intent,
        CandidateLayout::Preserved,
    );
    let new_state_preserved = persist_candidate_preserved(&new_state, CandidateOrigin::AlreadySatisfied);
    let new_state_expected = new_state_preserved.rollback_successor(None).unwrap();
    let new_state_database = new_state.fixture.database_snapshot();
    let new_state_namespace = new_state.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let new_state_error = enter_candidate(&new_state);

    assert_pending_phase(&new_state_error, Phase::FreshDbInvalidationIntent);
    assert_eq!(new_state.fixture.canonical_record(), new_state_expected);
    assert_eq!(new_state.fixture.database_snapshot(), new_state_database);
    assert_eq!(new_state.fixture.namespace_snapshot(), new_state_namespace);
    assert_no_candidate_effects();

    let active = build_active(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let active_preserved = persist_candidate_preserved(&active, CandidateOrigin::Applied);
    let active_complete = expected_rollback_complete(&active_preserved);
    let journal = active.open_journal();
    journal.advance(&active_preserved, &active_complete).unwrap();
    drop(journal);
    let active_database = active.fixture.database_snapshot();
    let active_namespace = active.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let active_clean = enter_clean_candidate(&active);

    assert_canonical_absent(&active.fixture.installation.root);
    assert_eq!(active.fixture.database_snapshot(), active_database);
    assert_eq!(active.fixture.namespace_snapshot(), active_namespace);
    assert_no_candidate_effects();
    drop(active_clean);

    let wrong_plan = build_active(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let mut inexact = persist_candidate_preserved(&wrong_plan, CandidateOrigin::Applied);
    let rollback = inexact.rollback.as_mut().unwrap();
    rollback.source = ForwardPhase::BootSyncStarted;
    assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
    rollback.boot = BootRollback::PendingUnverifiable;
    rollback.external_effects_may_remain = true;
    assert_eq!(
        inexact.rollback_successor(None).unwrap().phase,
        Phase::BootRepairRequired
    );
    fs::write(
        canonical_journal(&wrong_plan.fixture.installation.root),
        encode(&inexact).unwrap(),
    )
    .unwrap();
    let wrong_plan_database = wrong_plan.fixture.database_snapshot();
    let wrong_plan_namespace = wrong_plan.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let wrong_plan_error = enter_candidate(&wrong_plan);

    assert_pending_phase(&wrong_plan_error, Phase::CandidatePreserved);
    assert_eq!(wrong_plan.fixture.canonical_record(), inexact);
    assert_eq!(wrong_plan.fixture.database_snapshot(), wrong_plan_database);
    assert_eq!(wrong_plan.fixture.namespace_snapshot(), wrong_plan_namespace);
    assert_no_candidate_effects();

    // A valid generic rollback plan that also derives RollbackComplete must
    // not widen this operation-specific route beyond its two /usr sources.
    let route_inexact = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let mut completion_lookalike = persist_candidate_preserved(&route_inexact, CandidateOrigin::AlreadySatisfied);
    let rollback = completion_lookalike.rollback.as_mut().unwrap();
    rollback.source = ForwardPhase::TransactionTriggersComplete;
    rollback.usr_exchange = RollbackAction::NotRequired;
    assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);
    assert_eq!(rollback.fresh_db, RollbackAction::NotRequired);
    assert_eq!(rollback.boot, BootRollback::NotRequired);
    rollback.external_effects_may_remain = true;
    assert_eq!(
        completion_lookalike.rollback_successor(None).unwrap().phase,
        Phase::RollbackComplete
    );
    fs::write(
        canonical_journal(&route_inexact.fixture.installation.root),
        encode(&completion_lookalike).unwrap(),
    )
    .unwrap();
    let route_inexact_database = route_inexact.fixture.database_snapshot();
    let route_inexact_namespace = route_inexact.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let route_inexact_error = enter_candidate(&route_inexact);

    assert_pending_phase(&route_inexact_error, Phase::CandidatePreserved);
    assert_eq!(route_inexact.fixture.canonical_record(), completion_lookalike);
    assert_eq!(route_inexact.fixture.database_snapshot(), route_inexact_database);
    assert_eq!(route_inexact.fixture.namespace_snapshot(), route_inexact_namespace);
    assert_no_candidate_effects();

    let wrong_topology = build_active(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let wrong_topology_record = persist_candidate_preserved(&wrong_topology, CandidateOrigin::AlreadySatisfied);
    let exact_wrapper = active_wrapper_path(&wrong_topology);
    let lookalike = wrong_topology.fixture.installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-{}-{}-{WRAPPER_INDEX}",
        i32::from(wrong_topology.fixture.previous_state) + 1,
        wrong_topology_record.previous.tree_token.as_str()
    ));
    fs::rename(&exact_wrapper, &lookalike).unwrap();
    let wrong_topology_database = wrong_topology.fixture.database_snapshot();
    let wrong_topology_namespace = wrong_topology.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let wrong_topology_error = enter_candidate(&wrong_topology);

    assert_pending_phase(&wrong_topology_error, Phase::CandidatePreserved);
    assert_eq!(wrong_topology.fixture.canonical_record(), wrong_topology_record);
    assert_eq!(wrong_topology.fixture.database_snapshot(), wrong_topology_database);
    assert_eq!(wrong_topology.fixture.namespace_snapshot(), wrong_topology_namespace);
    assert!(!exact_wrapper.exists());
    assert!(lookalike.join("usr").is_dir());
    assert_pending_any_blocker(
        &wrong_topology_error,
        &[
            RecoveryBlocker::ActivationNamespaceRejected,
            RecoveryBlocker::PhaseNamespaceConflict,
        ],
    );
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
