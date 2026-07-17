//! Database, provenance, journal, and namespace races through startup entry.

use crate::{
    client::{
        startup_gate,
        startup_reconciliation::{
            RecoveryBlocker, active_reblit_candidate_preserve_exchange_attempt_count,
            arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture,
            arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence,
            arm_between_usr_rollback_candidate_preserve_database_captures,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
        },
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, assert_active_persistence_authority_error, assert_pending_phase, build_active,
        enter_candidate, expected_candidate_preserved,
    },
};

#[test]
fn startup_active_reblit_candidate_dispatch_rejects_database_provenance_journal_and_namespace_races() {
    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::Applied,
    );
    let source = fixture.candidate_intent.clone();
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_between_usr_rollback_candidate_preserve_database_captures(move || {
        database.remove(&candidate).unwrap();
    });

    let database_error = enter_candidate(&fixture);

    assert_pending_phase(&database_error, Phase::CandidatePreserveIntent);
    assert_pending_blocker(&database_error, RecoveryBlocker::DatabaseConflict);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);

    let fixture = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::Applied,
    );
    let source = fixture.candidate_intent.clone();
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_between_usr_rollback_candidate_preserve_database_captures(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });

    let provenance_error = enter_candidate(&fixture);

    assert_pending_phase(&provenance_error, Phase::CandidatePreserveIntent);
    assert_pending_blocker(&provenance_error, RecoveryBlocker::MetadataProvenanceConflict);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);

    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = fixture.candidate_intent.clone();
    let expected_changed = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence(fixture.journal_change_hook());

    let journal_error = enter_candidate(&fixture);

    assert_active_persistence_authority_error(&journal_error);
    assert_ne!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.canonical_record(), expected_changed);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);

    let fixture = build_active(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = fixture.candidate_intent.clone();
    let database_before = fixture.fixture.database_snapshot();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture(
        fixture.namespace_change_hook("active-reblit-startup-race".to_owned()),
    );

    let namespace_error = enter_candidate(&fixture);

    assert_active_persistence_authority_error(&namespace_error);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
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
