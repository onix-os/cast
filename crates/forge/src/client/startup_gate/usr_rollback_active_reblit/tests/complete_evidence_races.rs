//! Database, provenance, journal, and namespace races at completion routing.

use crate::{
    client::{
        startup_gate,
        startup_reconciliation::{
            RecoveryBlocker, arm_before_usr_rollback_active_reblit_complete_route_fresh_namespace_capture,
            arm_between_usr_rollback_active_reblit_complete_route_database_captures,
        },
        startup_recovery::arm_before_usr_rollback_active_reblit_complete_route_final_revalidation,
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_complete_persistence_authority_error,
        assert_no_candidate_effects, build_active, enter_candidate, expected_candidate_preserved,
        persist_candidate_preserved, reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_complete_route_rejects_database_provenance_journal_and_namespace_races() {
    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    reset_candidate_effect_observers();
    arm_between_usr_rollback_active_reblit_complete_route_database_captures(move || {
        database.remove(&candidate).unwrap();
    });

    let database_error = enter_candidate(&fixture);

    assert_pending_blocker(&database_error, RecoveryBlocker::DatabaseConflict);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert!(fixture.fixture.database.get(candidate).is_err());
    assert_no_candidate_effects();

    let fixture = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    reset_candidate_effect_observers();
    arm_between_usr_rollback_active_reblit_complete_route_database_captures(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });

    let provenance_error = enter_candidate(&fixture);

    assert_pending_blocker(&provenance_error, RecoveryBlocker::MetadataProvenanceConflict);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert!(
        fixture
            .fixture
            .database
            .metadata_provenance(candidate)
            .unwrap()
            .is_none()
    );
    assert_no_candidate_effects();

    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
    let changed = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_complete_route_final_revalidation(fixture.journal_change_hook());

    let journal_error = enter_candidate(&fixture);

    assert_complete_persistence_authority_error(&journal_error);
    assert_ne!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.canonical_record(), changed);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    let fixture = build_active(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let database_before = fixture.fixture.database_snapshot();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_complete_route_fresh_namespace_capture(
        fixture.namespace_change_hook("active-reblit-complete-route-race".to_owned()),
    );

    let namespace_error = enter_candidate(&fixture);

    assert_complete_persistence_authority_error(&namespace_error);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert!(
        fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("active-reblit-complete-route-race")
            .is_dir()
    );
    assert!(active_wrapper_path(&fixture).join("usr").is_dir());
    assert_no_candidate_effects();
}

fn assert_pending_blocker(error: &startup_gate::Error, blocker: RecoveryBlocker) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected recovery-pending blocker {blocker:?}, got {error:?}");
    };
    assert!(
        pending.blockers().contains(&blocker),
        "expected blocker {blocker:?}, got {:?}",
        pending.blockers()
    );
}
