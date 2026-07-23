//! Clean-startup handoff and post-finalization database re-audit contracts.

use std::{
    fs,
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::startup_gate::{self, arm_after_usr_rollback_finalization_before_clean_audit},
    transition_journal::{
        Operation, Phase, RollbackActionOutcome, StorageError, TransitionJournalStore, TransitionRecord, encode,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, FreshOutcome, assert_canonical_absent, build_fresh_invalidation, effect_counts,
        enter_invalidation, persist_fresh_invalidated, persist_rollback_complete, reset_namespace_effect_counts,
        retain_clean_invalidation,
    },
};

#[test]
fn startup_new_state_suffix_terminal_handoff_retains_the_same_journal_lock_through_clean_startup() {
    let fixture = build_fresh_invalidation(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
        FreshOutcome::AlreadySatisfied,
    );
    let invalidated = persist_fresh_invalidated(&fixture, FreshOutcome::Applied);
    let _complete = persist_rollback_complete(&fixture, &invalidated);
    reset_namespace_effect_counts();
    let removal_before = effect_counts().fresh_removal;

    let clean = retain_clean_invalidation(&fixture);

    let installation = &fixture.fixture.fixture.installation;
    assert_canonical_absent(&installation.root);
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let error = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root).unwrap_err();
    assert!(matches!(error, StorageError::AcquireLock { .. }), "{error:?}");
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, removal_before);

    drop(clean);
    let reopened = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root).unwrap();
    assert_eq!(reopened.load().unwrap(), None);
}

#[test]
fn startup_new_state_suffix_reaudits_database_after_finalization_before_clean_admission() {
    let fixture = build_fresh_invalidation(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::Applied,
        FreshOutcome::AlreadySatisfied,
    );
    let invalidated = persist_fresh_invalidated(&fixture, FreshOutcome::AlreadySatisfied);
    let complete = persist_rollback_complete(&fixture, &invalidated);
    let database = fixture.fixture.fixture.database.clone();
    let transition = complete.transition_id.clone();
    let namespace_before = fixture.namespace_snapshot();
    reset_namespace_effect_counts();
    let removal_before = effect_counts().fresh_removal;
    arm_after_usr_rollback_finalization_before_clean_audit({
        let transition = transition.clone();
        move || {
            database
                .add_with_transition(&transition, &[], Some("post-finalization orphan"), None)
                .unwrap();
        }
    });

    let error = enter_invalidation(&fixture);

    assert!(
        matches!(
            &error,
            startup_gate::Error::OrphanTransitionRow {
                transition,
                ..
            } if transition == complete.transition_id.as_str()
        ),
        "{error:?}"
    );
    assert_canonical_absent(&fixture.fixture.fixture.installation.root);
    assert!(
        fixture
            .fixture
            .fixture
            .database
            .audit_in_flight_transition()
            .unwrap()
            .is_some()
    );
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, removal_before);
}

#[test]
fn startup_new_state_suffix_finalization_converges_into_the_shared_prune_residue_audit() {
    let fixture = build_fresh_invalidation(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateOutcome::AlreadySatisfied,
        FreshOutcome::AlreadySatisfied,
    );
    let invalidated = persist_fresh_invalidated(&fixture, FreshOutcome::AlreadySatisfied);
    let _complete = persist_rollback_complete(&fixture, &invalidated);
    let residue = fixture
        .fixture
        .fixture
        .installation
        .state_quarantine_dir()
        .join("state-prune-991-finalization-test");
    let inserted = residue.clone();
    reset_namespace_effect_counts();
    let removal_before = effect_counts().fresh_removal;
    arm_after_usr_rollback_finalization_before_clean_audit(move || fs::create_dir(inserted).unwrap());

    let error = enter_invalidation(&fixture);

    assert!(
        matches!(
            &error,
            startup_gate::Error::ArchivedStatePruneResidue(
                crate::transition_identity::ArchivedStatePruneResidueError::Residue { path }
            ) if path == &residue
        ),
        "{error:?}"
    );
    assert_canonical_absent(&fixture.fixture.fixture.installation.root);
    assert!(residue.is_dir());
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, removal_before);
}

#[test]
fn startup_new_state_suffix_rejects_terminal_record_recreated_during_clean_handoff() {
    let fixture = build_fresh_invalidation(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
        FreshOutcome::AlreadySatisfied,
    );
    let invalidated = persist_fresh_invalidated(&fixture, FreshOutcome::Applied);
    let complete = persist_rollback_complete(&fixture, &invalidated);
    let canonical = fixture
        .fixture
        .fixture
        .installation
        .root
        .join(".cast/journal/state-transition");
    let database_before = fixture.fixture.fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    let recreated = complete.clone();
    reset_namespace_effect_counts();
    let removal_before = effect_counts().fresh_removal;
    arm_after_usr_rollback_finalization_before_clean_audit(move || write_new_private_record(&canonical, &recreated));

    let error = enter_invalidation(&fixture);

    assert!(
        matches!(
            &error,
            startup_gate::Error::CanonicalTransitionAppearedDuringCleanAdmission {
                transition,
                operation: Operation::NewState,
                phase: Phase::RollbackComplete,
            } if transition == complete.transition_id.as_str()
        ),
        "{error:?}"
    );
    assert_eq!(fixture.canonical_record(), complete);
    assert_eq!(fixture.fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, removal_before);
}

#[test]
fn startup_new_state_suffix_rejects_mutable_namespace_substitution_after_terminal_finalization() {
    let fixture = build_fresh_invalidation(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
        FreshOutcome::AlreadySatisfied,
    );
    let invalidated = persist_fresh_invalidated(&fixture, FreshOutcome::AlreadySatisfied);
    let _complete = persist_rollback_complete(&fixture, &invalidated);
    let root = fixture.fixture.fixture.installation.root.clone();
    let cast = root.join(".cast");
    let displaced = root.join(".cast-displaced-finalization-test");
    let callback_cast = cast.clone();
    let callback_displaced = displaced.clone();
    let database_before = fixture.fixture.fixture.database_snapshot();
    reset_namespace_effect_counts();
    let removal_before = effect_counts().fresh_removal;
    arm_after_usr_rollback_finalization_before_clean_audit(move || {
        fs::rename(callback_cast, &callback_displaced).unwrap();
        fs::create_dir(&cast).unwrap();
        fs::set_permissions(&cast, fs::Permissions::from_mode(0o700)).unwrap();
    });

    let error = enter_invalidation(&fixture);

    assert!(matches!(&error, startup_gate::Error::Installation(_)), "{error:?}");
    assert!(!displaced.join("journal/state-transition").exists());
    assert_eq!(fixture.fixture.fixture.database_snapshot(), database_before);
    assert_eq!(effect_counts().candidate_move, 0);
    assert_eq!(effect_counts().fresh_removal, removal_before);
}

fn write_new_private_record(path: &Path, record: &TransitionRecord) {
    let encoded = encode(record).unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o600)).unwrap();
    file.write_all(&encoded).unwrap();
    file.sync_all().unwrap();
}
