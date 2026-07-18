//! Database, provenance, journal, and namespace races at terminal finalization.

use std::fs;

use crate::{
    client::{
        startup_gate,
        startup_reconciliation::{
            RecoveryBlocker, arm_before_usr_rollback_active_reblit_finalization_fresh_namespace_capture,
            arm_between_usr_rollback_active_reblit_finalization_database_captures,
        },
        startup_recovery::{
            arm_after_usr_rollback_active_reblit_finalization_delete,
            arm_before_usr_rollback_active_reblit_finalization_final_revalidation,
        },
    },
    transition_journal::{RollbackActionOutcome, encode},
};

use super::{
    super::{candidate_test_support::CandidateSource, test_fixture::canonical_journal},
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_canonical_absent, assert_no_candidate_effects,
        build_active, enter_candidate, expected_candidate_preserved, persist_rollback_complete,
        reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_finalization_refuses_wrong_database_and_provenance_evidence() {
    let missing = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&missing, CandidateOrigin::Applied);
    let namespace_before = missing.fixture.namespace_snapshot();
    missing
        .fixture
        .database
        .remove(&missing.fixture.candidate_state)
        .unwrap();
    let database_after = missing.fixture.database_snapshot();
    reset_candidate_effect_observers();

    let missing_error = enter_candidate(&missing);

    assert_pending_blocker(&missing_error, RecoveryBlocker::DatabaseConflict);
    assert_eq!(missing.fixture.canonical_record(), terminal);
    assert_eq!(missing.fixture.database_snapshot(), database_after);
    assert_eq!(missing.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    let no_provenance = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&no_provenance, CandidateOrigin::AlreadySatisfied);
    let namespace_before = no_provenance.fixture.namespace_snapshot();
    no_provenance
        .fixture
        .database
        .delete_metadata_provenance_for_test(no_provenance.fixture.candidate_state)
        .unwrap();
    let database_after = no_provenance.fixture.database_snapshot();
    reset_candidate_effect_observers();

    let provenance_error = enter_candidate(&no_provenance);

    assert_pending_blocker(&provenance_error, RecoveryBlocker::MetadataProvenanceConflict);
    assert_eq!(no_provenance.fixture.canonical_record(), terminal);
    assert_eq!(no_provenance.fixture.database_snapshot(), database_after);
    assert_eq!(no_provenance.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();
}

#[test]
fn startup_active_reblit_finalization_rejects_capture_and_final_pre_evidence_races() {
    let capture_database = build_active(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&capture_database, CandidateOrigin::Applied);
    let namespace_before = capture_database.fixture.namespace_snapshot();
    let database = capture_database.fixture.database.clone();
    let candidate = capture_database.fixture.candidate_state;
    reset_candidate_effect_observers();
    arm_between_usr_rollback_active_reblit_finalization_database_captures(move || {
        database.remove(&candidate).unwrap();
    });

    let capture_database_error = enter_candidate(&capture_database);

    assert_pending_blocker(&capture_database_error, RecoveryBlocker::DatabaseConflict);
    assert_eq!(capture_database.fixture.canonical_record(), terminal);
    assert_eq!(capture_database.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    let capture_namespace = build_active(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&capture_namespace, CandidateOrigin::AlreadySatisfied);
    let database_before = capture_namespace.fixture.database_snapshot();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_finalization_fresh_namespace_capture(
        capture_namespace.namespace_change_hook("active-reblit-finalization-capture-race".to_owned()),
    );

    let capture_namespace_error = enter_candidate(&capture_namespace);

    assert_active_dispatch_error(&capture_namespace_error);
    assert_eq!(capture_namespace.fixture.canonical_record(), terminal);
    assert_eq!(capture_namespace.fixture.database_snapshot(), database_before);
    assert!(
        capture_namespace
            .fixture
            .installation
            .state_quarantine_dir()
            .join("active-reblit-finalization-capture-race")
            .is_dir()
    );
    assert_no_candidate_effects();

    let final_journal = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&final_journal, CandidateOrigin::Applied);
    let preterminal = expected_candidate_preserved(&final_journal, CandidateOrigin::Applied);
    let canonical = canonical_journal(&final_journal.fixture.installation.root);
    let bytes = encode(&preterminal).unwrap();
    let database_before = final_journal.fixture.database_snapshot();
    let namespace_before = final_journal.fixture.namespace_snapshot();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_finalization_final_revalidation(move || {
        fs::write(canonical, bytes).unwrap();
    });

    let final_journal_error = enter_candidate(&final_journal);

    assert!(
        matches!(
            final_journal_error,
            startup_gate::Error::UsrRollbackActiveReblitDispatch(_)
        ),
        "{final_journal_error:?}"
    );
    assert_ne!(final_journal.fixture.canonical_record(), terminal);
    assert_eq!(final_journal.fixture.canonical_record(), preterminal);
    assert_eq!(final_journal.fixture.database_snapshot(), database_before);
    assert_eq!(final_journal.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    let final_namespace = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let terminal = persist_rollback_complete(&final_namespace, CandidateOrigin::AlreadySatisfied);
    let database_before = final_namespace.fixture.database_snapshot();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_finalization_final_revalidation(
        final_namespace.namespace_change_hook("active-reblit-finalization-final-race".to_owned()),
    );

    let final_namespace_error = enter_candidate(&final_namespace);

    assert!(
        matches!(
            final_namespace_error,
            startup_gate::Error::UsrRollbackActiveReblitDispatch(_)
        ),
        "{final_namespace_error:?}"
    );
    assert_eq!(final_namespace.fixture.canonical_record(), terminal);
    assert_eq!(final_namespace.fixture.database_snapshot(), database_before);
    assert_no_candidate_effects();
}

#[test]
fn startup_active_reblit_finalization_rejects_database_and_provenance_changes_at_final_pre_and_after_delete() {
    for provenance in [false, true] {
        let fixture = build_active(
            Epoch::Current,
            CandidateSource::Intent,
            RollbackActionOutcome::Applied,
            CandidateOrigin::AlreadySatisfied,
        );
        let terminal = persist_rollback_complete(&fixture, CandidateOrigin::Applied);
        let database = fixture.fixture.database.clone();
        let candidate = fixture.fixture.candidate_state;
        let namespace_before = fixture.fixture.namespace_snapshot();
        reset_candidate_effect_observers();
        arm_before_usr_rollback_active_reblit_finalization_final_revalidation(move || {
            if provenance {
                database.delete_metadata_provenance_for_test(candidate).unwrap();
            } else {
                database.remove(&candidate).unwrap();
            }
        });

        let error = enter_candidate(&fixture);

        assert_active_dispatch_error(&error);
        assert_eq!(fixture.fixture.canonical_record(), terminal);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_no_candidate_effects();
    }

    for provenance in [false, true] {
        let fixture = build_active(
            Epoch::Historical,
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOrigin::AlreadySatisfied,
        );
        let _terminal = persist_rollback_complete(&fixture, CandidateOrigin::AlreadySatisfied);
        let database = fixture.fixture.database.clone();
        let candidate = fixture.fixture.candidate_state;
        let namespace_before = fixture.fixture.namespace_snapshot();
        reset_candidate_effect_observers();
        arm_after_usr_rollback_active_reblit_finalization_delete(move || {
            if provenance {
                database.delete_metadata_provenance_for_test(candidate).unwrap();
            } else {
                database.remove(&candidate).unwrap();
            }
        });

        let error = enter_candidate(&fixture);

        assert_active_dispatch_error(&error);
        assert_canonical_absent(&fixture.fixture.installation.root);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_no_candidate_effects();
    }

    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let _terminal = persist_rollback_complete(&fixture, CandidateOrigin::Applied);
    let wrapper = active_wrapper_path(&fixture);
    let displaced = fixture
        .fixture
        .installation
        .state_quarantine_dir()
        .join("active-reblit-finalization-post-delete-wrapper-race");
    let hook_wrapper = wrapper.clone();
    let hook_displaced = displaced.clone();
    let database_before = fixture.fixture.database_snapshot();
    reset_candidate_effect_observers();
    arm_after_usr_rollback_active_reblit_finalization_delete(move || {
        fs::rename(hook_wrapper, &hook_displaced).unwrap();
    });

    let error = enter_candidate(&fixture);

    assert_active_dispatch_error(&error);
    assert_canonical_absent(&fixture.fixture.installation.root);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert!(!wrapper.exists());
    assert!(displaced.join("usr").is_dir());
    assert_no_candidate_effects();
}

fn assert_pending_blocker(error: &startup_gate::Error, expected: RecoveryBlocker) {
    assert_pending_phase_with_any_blocker(error, &[expected]);
}

fn assert_pending_phase_with_any_blocker(error: &startup_gate::Error, expected: &[RecoveryBlocker]) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected recovery-pending refusal, got {error:?}");
    };
    assert!(
        expected.iter().any(|blocker| pending.blockers().contains(blocker)),
        "expected one of {expected:?}, got {:?}",
        pending.blockers()
    );
}

fn assert_active_dispatch_error(error: &startup_gate::Error) {
    assert!(
        matches!(error, startup_gate::Error::UsrRollbackActiveReblitDispatch(_)),
        "expected typed ActiveReblit finalization dispatch error, got {error:?}"
    );
}
