//! Database, provenance, journal, and namespace races at terminal finalization.

use std::fs;

use crate::{
    client::{
        startup_gate,
        startup_reconciliation::{
            RecoveryBlocker, arm_before_usr_rollback_activate_archived_finalization_fresh_namespace_capture,
            arm_between_usr_rollback_activate_archived_finalization_database_captures,
        },
        startup_recovery::{
            arm_after_usr_rollback_activate_archived_finalization_delete,
            arm_before_usr_rollback_activate_archived_finalization_final_revalidation,
        },
    },
    transition_journal::{RollbackActionOutcome, encode},
};

use super::{
    super::{candidate_test_support::CandidateSource, test_fixture::canonical_journal},
    support::{
        CandidateOutcome, Epoch, RouteFixture, assert_canonical_absent, assert_finalization_dispatch_error,
        candidate_move_count, enter_route, persist_rollback_complete, reset_candidate_observers,
    },
};

#[derive(Clone, Copy)]
enum MissingEvidence {
    Candidate,
    Previous,
    Provenance,
}

#[test]
fn startup_activate_archived_finalization_requires_exact_two_rows_and_candidate_provenance() {
    for missing in [
        MissingEvidence::Candidate,
        MissingEvidence::Previous,
        MissingEvidence::Provenance,
    ] {
        let fixture = RouteFixture::new(
            Epoch::Current,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOutcome::Applied,
        );
        let terminal = persist_rollback_complete(&fixture);
        let namespace_before = fixture.namespace_snapshot();
        match missing {
            MissingEvidence::Candidate => fixture
                .fixture
                .fixture
                .database
                .remove(&fixture.fixture.fixture.candidate_state)
                .unwrap(),
            MissingEvidence::Previous => fixture
                .fixture
                .fixture
                .database
                .remove(&fixture.fixture.fixture.previous_state)
                .unwrap(),
            MissingEvidence::Provenance => fixture
                .fixture
                .fixture
                .database
                .delete_metadata_provenance_for_test(fixture.fixture.fixture.candidate_state)
                .unwrap(),
        }
        let database_after = fixture.database_snapshot();
        reset_candidate_observers();

        let error = enter_route(&fixture);

        let expected = match missing {
            MissingEvidence::Provenance => RecoveryBlocker::MetadataProvenanceConflict,
            MissingEvidence::Candidate | MissingEvidence::Previous => RecoveryBlocker::DatabaseConflict,
        };
        assert_pending_blocker(&error, expected);
        assert_eq!(fixture.canonical_record(), terminal);
        assert_eq!(fixture.database_snapshot(), database_after);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
        assert_eq!(candidate_move_count(), 0);
    }
}

#[test]
fn startup_activate_archived_finalization_rejects_capture_and_final_pre_races() {
    let capture_database = exact_route(Epoch::Current);
    let terminal = persist_rollback_complete(&capture_database);
    let namespace_before = capture_database.namespace_snapshot();
    let database = capture_database.fixture.fixture.database.clone();
    let candidate = capture_database.fixture.fixture.candidate_state;
    reset_candidate_observers();
    arm_between_usr_rollback_activate_archived_finalization_database_captures(move || {
        database.remove(&candidate).unwrap();
    });

    let capture_database_error = enter_route(&capture_database);

    assert_pending_blocker(&capture_database_error, RecoveryBlocker::DatabaseConflict);
    assert_eq!(capture_database.canonical_record(), terminal);
    assert_eq!(capture_database.namespace_snapshot(), namespace_before);
    assert_eq!(candidate_move_count(), 0);

    let capture_namespace = exact_route(Epoch::Historical);
    let terminal = persist_rollback_complete(&capture_namespace);
    let database_before = capture_namespace.database_snapshot();
    arm_before_usr_rollback_activate_archived_finalization_fresh_namespace_capture(
        capture_namespace
            .fixture
            .namespace_change_hook("activate-archived-finalization-capture-race".to_owned()),
    );

    let capture_namespace_error = enter_route(&capture_namespace);

    assert_finalization_dispatch_error(&capture_namespace_error);
    assert_eq!(capture_namespace.canonical_record(), terminal);
    assert_eq!(capture_namespace.database_snapshot(), database_before);

    let final_journal = exact_route(Epoch::Current);
    let terminal = persist_rollback_complete(&final_journal);
    let canonical = canonical_journal(&final_journal.fixture.fixture.installation.root);
    let bytes = encode(&final_journal.source).unwrap();
    let database_before = final_journal.database_snapshot();
    let namespace_before = final_journal.namespace_snapshot();
    arm_before_usr_rollback_activate_archived_finalization_final_revalidation(move || {
        fs::write(canonical, bytes).unwrap();
    });

    let final_journal_error = enter_route(&final_journal);

    assert_finalization_dispatch_error(&final_journal_error);
    assert_ne!(final_journal.canonical_record(), terminal);
    assert_eq!(final_journal.canonical_record(), final_journal.source);
    assert_eq!(final_journal.database_snapshot(), database_before);
    assert_eq!(final_journal.namespace_snapshot(), namespace_before);

    let final_namespace = exact_route(Epoch::Historical);
    let terminal = persist_rollback_complete(&final_namespace);
    let database_before = final_namespace.database_snapshot();
    arm_before_usr_rollback_activate_archived_finalization_final_revalidation(
        final_namespace
            .fixture
            .namespace_change_hook("activate-archived-finalization-final-race".to_owned()),
    );

    let final_namespace_error = enter_route(&final_namespace);

    assert_finalization_dispatch_error(&final_namespace_error);
    assert_eq!(final_namespace.canonical_record(), terminal);
    assert_eq!(final_namespace.database_snapshot(), database_before);
}

#[test]
fn startup_activate_archived_finalization_rejects_final_pre_and_post_delete_evidence_changes() {
    for provenance in [false, true] {
        let fixture = exact_route(Epoch::Current);
        let terminal = persist_rollback_complete(&fixture);
        let database = fixture.fixture.fixture.database.clone();
        let candidate = fixture.fixture.fixture.candidate_state;
        let namespace_before = fixture.namespace_snapshot();
        arm_before_usr_rollback_activate_archived_finalization_final_revalidation(move || {
            if provenance {
                database.delete_metadata_provenance_for_test(candidate).unwrap();
            } else {
                database.remove(&candidate).unwrap();
            }
        });

        let error = enter_route(&fixture);

        assert_finalization_dispatch_error(&error);
        assert_eq!(fixture.canonical_record(), terminal);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
    }

    for provenance in [false, true] {
        let fixture = exact_route(Epoch::Historical);
        let _terminal = persist_rollback_complete(&fixture);
        let database = fixture.fixture.fixture.database.clone();
        let candidate = fixture.fixture.fixture.candidate_state;
        let namespace_before = fixture.namespace_snapshot();
        arm_after_usr_rollback_activate_archived_finalization_delete(move || {
            if provenance {
                database.delete_metadata_provenance_for_test(candidate).unwrap();
            } else {
                database.remove(&candidate).unwrap();
            }
        });

        let error = enter_route(&fixture);

        assert_finalization_dispatch_error(&error);
        assert_canonical_absent(&fixture.fixture.fixture.installation.root);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
    }

    let fixture = exact_route(Epoch::Current);
    let _terminal = persist_rollback_complete(&fixture);
    let wrapper = fixture.archived_wrapper_path();
    let displaced = fixture
        .fixture
        .fixture
        .installation
        .state_quarantine_dir()
        .join("activate-archived-finalization-post-delete-wrapper-race");
    let hook_wrapper = wrapper.clone();
    let hook_displaced = displaced.clone();
    let database_before = fixture.database_snapshot();
    arm_after_usr_rollback_activate_archived_finalization_delete(move || {
        fs::rename(hook_wrapper, &hook_displaced).unwrap();
    });

    let error = enter_route(&fixture);

    assert_finalization_dispatch_error(&error);
    assert_canonical_absent(&fixture.fixture.fixture.installation.root);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert!(!wrapper.exists());
    assert!(displaced.join("usr").is_dir());
    assert_eq!(candidate_move_count(), 0);
}

fn exact_route(epoch: Epoch) -> RouteFixture {
    RouteFixture::new(
        epoch,
        CandidateSource::RootLinksComplete,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
    )
}

fn assert_pending_blocker(error: &startup_gate::Error, expected: RecoveryBlocker) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected recovery-pending refusal, got {error:?}");
    };
    assert!(
        pending.blockers().contains(&expected),
        "expected {expected:?}, got {:?}",
        pending.blockers()
    );
}
