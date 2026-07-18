//! Phase ordering, exact-plan, topology, and operation isolation at finalization.

use std::fs;

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackActivateArchivedFinalizationSeal,
        startup_reconciliation::{
            UsrRollbackActivateArchivedFinalizationAdmission, UsrRollbackActivateArchivedFinalizationAuthority,
        },
    },
    transition_journal::{ForwardPhase, Operation, Phase, RollbackAction, RollbackActionOutcome, encode},
};

use super::{
    super::{candidate_test_support::CandidateSource, test_fixture::canonical_journal},
    support::{
        CandidateOutcome, Epoch, RouteFixture, assert_canonical_absent, assert_pending_phase,
        assert_route_pending_audit, candidate_move_count, enter_clean_route, enter_route, persist_rollback_complete,
        reset_candidate_observers,
    },
};

#[test]
fn startup_activate_archived_finalization_keeps_completion_and_terminal_deletion_on_separate_entries() {
    let fixture = RouteFixture::new(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
    );
    let complete = fixture.expected_successor();
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    reset_candidate_observers();

    let first = enter_route(&fixture);

    assert_route_pending_audit(&first, &fixture, &complete);
    assert_eq!(fixture.canonical_record(), complete);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(candidate_move_count(), 0);

    let clean = enter_clean_route(&fixture);

    assert_canonical_absent(&fixture.fixture.fixture.installation.root);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(candidate_move_count(), 0);
    drop(clean);
}

#[test]
fn startup_activate_archived_finalization_rejects_valid_terminal_lookalike_plan_and_wrong_topology() {
    let lookalike = RouteFixture::new(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
    );
    let mut inexact_source = lookalike.source.clone();
    let rollback = inexact_source.rollback.as_mut().unwrap();
    rollback.source = ForwardPhase::PreviousArchiveIntent;
    rollback.previous_archive = RollbackAction::AlreadySatisfied;
    rollback.external_effects_may_remain = true;
    let terminal_lookalike = inexact_source.rollback_successor(None).unwrap();
    assert_eq!(terminal_lookalike.phase, Phase::RollbackComplete);
    fs::write(
        canonical_journal(&lookalike.fixture.fixture.installation.root),
        encode(&terminal_lookalike).unwrap(),
    )
    .unwrap();
    let database_before = lookalike.database_snapshot();
    let namespace_before = lookalike.namespace_snapshot();
    reset_candidate_observers();

    let plan_error = enter_route(&lookalike);

    assert_pending_phase(&plan_error, Phase::RollbackComplete);
    assert_eq!(lookalike.canonical_record(), terminal_lookalike);
    assert_eq!(lookalike.database_snapshot(), database_before);
    assert_eq!(lookalike.namespace_snapshot(), namespace_before);
    assert_eq!(candidate_move_count(), 0);

    let topology = RouteFixture::new(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
    );
    let terminal = persist_rollback_complete(&topology);
    fs::remove_file(topology.archived_slot_path()).unwrap();
    let database_before = topology.database_snapshot();
    let namespace_after = topology.namespace_snapshot();
    reset_candidate_observers();

    let topology_error = enter_route(&topology);

    assert_pending_phase(&topology_error, Phase::RollbackComplete);
    assert_eq!(topology.canonical_record(), terminal);
    assert_eq!(topology.database_snapshot(), database_before);
    assert_eq!(topology.namespace_snapshot(), namespace_after);
    assert_eq!(candidate_move_count(), 0);
}

#[test]
fn startup_activate_archived_finalization_authority_excludes_other_operations_and_phases() {
    let fixture = RouteFixture::new(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&fixture);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let seal = UsrRollbackActivateArchivedFinalizationSeal::new_for_test();

    assert!(matches!(
        UsrRollbackActivateArchivedFinalizationAuthority::capture(
            &seal,
            &fixture.fixture.fixture.installation,
            &journal,
            &fixture.fixture.fixture.database,
            &reservation,
            &fixture.source,
        )
        .unwrap(),
        UsrRollbackActivateArchivedFinalizationAdmission::NotApplicable
    ));
    for operation in [Operation::NewState, Operation::ActiveReblit] {
        let mut other = terminal.clone();
        other.operation = operation;
        assert!(matches!(
            UsrRollbackActivateArchivedFinalizationAuthority::capture(
                &seal,
                &fixture.fixture.fixture.installation,
                &journal,
                &fixture.fixture.fixture.database,
                &reservation,
                &other,
            )
            .unwrap(),
            UsrRollbackActivateArchivedFinalizationAdmission::NotApplicable
        ));
    }
    assert_eq!(fixture.canonical_record(), terminal);
}
