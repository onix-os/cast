//! Adversarial substitution of the public journal directory and lock name.

use std::{
    fs,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            UsrRollbackFinalizationError, UsrRollbackFinalizationVerificationError,
            arm_after_usr_rollback_finalization_delete, arm_before_usr_rollback_finalization_final_revalidation,
            finalize_usr_rollback,
        },
    },
    transition_journal::{RollbackActionOutcome, StorageError},
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Timing {
    BeforeDelete,
    AfterDelete,
}

#[test]
fn startup_usr_rollback_finalization_rejects_public_journal_directory_substitution() {
    for timing in [Timing::BeforeDelete, Timing::AfterDelete] {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::Applied,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::Applied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let root = fixture.installation().root.clone();
        let displaced = root.join(".cast/journal-displaced-by-finalization-test");
        let hook_displaced = displaced.clone();
        arm_at(timing, move || substitute_journal_directory(&root, &hook_displaced));

        let error = finalize_usr_rollback(journal, authority).unwrap_err();

        assert_binding_error(timing, &error, true);
        let displaced_record = displaced.join("state-transition");
        assert_eq!(displaced_record.exists(), timing == Timing::BeforeDelete);
        assert!(
            !fixture
                .installation()
                .root
                .join(".cast/journal/state-transition")
                .exists()
        );
        assert_eq!(
            fs::read_dir(fixture.installation().root.join(".cast/journal"))
                .unwrap()
                .count(),
            1
        );
        fixture.assert_no_second_removal();
    }
}

#[test]
fn startup_usr_rollback_finalization_rejects_public_journal_lock_substitution() {
    for timing in [Timing::BeforeDelete, Timing::AfterDelete] {
        let fixture = FinalizationFixture::historical(
            FreshDbOutcome::AlreadySatisfied,
            Source::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateResult::AlreadySatisfied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let root = fixture.installation().root.clone();
        let displaced = root.join(".cast/journal-lock-displaced-by-finalization-test");
        let hook_displaced = displaced.clone();
        arm_at(timing, move || substitute_journal_lock(&root, &hook_displaced));

        let error = finalize_usr_rollback(journal, authority).unwrap_err();

        assert_binding_error(timing, &error, false);
        assert!(displaced.is_file());
        assert_eq!(
            fixture
                .installation()
                .root
                .join(".cast/journal/state-transition")
                .exists(),
            timing == Timing::BeforeDelete
        );
        fixture.assert_no_second_removal();
    }
}

#[test]
fn startup_usr_rollback_finalization_rejects_hidden_canonical_record_displacement() {
    for timing in [Timing::BeforeDelete, Timing::AfterDelete] {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::Applied,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::AlreadySatisfied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let canonical = fixture.installation().root.join(".cast/journal/state-transition");
        let displaced = fixture
            .installation()
            .root
            .join(".cast/journal/state-transition-displaced-by-finalization-test");
        let exact_bytes = fs::read(&canonical).unwrap();
        let hook_canonical = canonical.clone();
        let hook_displaced = displaced.clone();
        let hook_bytes = exact_bytes.clone();
        arm_at(timing, move || match timing {
            Timing::BeforeDelete => {
                fs::rename(&hook_canonical, &hook_displaced).unwrap();
                write_new_private_file(&hook_canonical, &hook_bytes);
            }
            Timing::AfterDelete => write_new_private_file(&hook_displaced, &hook_bytes),
        });

        let error = finalize_usr_rollback(journal, authority).unwrap_err();

        assert_entry_set_error(timing, &error);
        assert_eq!(fs::read(&displaced).unwrap(), exact_bytes);
        if timing == Timing::BeforeDelete {
            assert_eq!(fs::read(&canonical).unwrap(), exact_bytes);
        } else {
            assert!(!canonical.exists());
        }
        fixture.assert_no_second_removal();
    }
}

fn arm_at(timing: Timing, hook: impl FnOnce() + 'static) {
    match timing {
        Timing::BeforeDelete => arm_before_usr_rollback_finalization_final_revalidation(hook),
        Timing::AfterDelete => arm_after_usr_rollback_finalization_delete(hook),
    }
}

fn assert_binding_error(timing: Timing, error: &UsrRollbackFinalizationError, directory: bool) {
    let binding_error = |source: &UsrRollbackFinalizationVerificationError| {
        matches!(
            source,
            UsrRollbackFinalizationVerificationError::Journal(StorageError::JournalDirectoryBindingChanged)
                if directory
        ) || matches!(
            source,
            UsrRollbackFinalizationVerificationError::Journal(StorageError::JournalLockBindingChanged)
                if !directory
        )
    };
    let matches_timing = match (timing, error) {
        (Timing::BeforeDelete, UsrRollbackFinalizationError::PreDeleteVerification(source)) => binding_error(source),
        (Timing::AfterDelete, UsrRollbackFinalizationError::PostDeleteVerification(source)) => binding_error(source),
        _ => false,
    };
    assert!(matches_timing, "timing={timing:?}, directory={directory}: {error:?}");
}

fn assert_entry_set_error(timing: Timing, error: &UsrRollbackFinalizationError) {
    let matches_timing = matches!(
        (timing, error),
        (
            Timing::BeforeDelete,
            UsrRollbackFinalizationError::PreDeleteVerification(UsrRollbackFinalizationVerificationError::Journal(
                StorageError::JournalEntrySetMismatch { .. }
            ))
        ) | (
            Timing::AfterDelete,
            UsrRollbackFinalizationError::PostDeleteVerification(UsrRollbackFinalizationVerificationError::Journal(
                StorageError::JournalEntrySetMismatch { .. }
            ))
        )
    );
    assert!(matches_timing, "timing={timing:?}: {error:?}");
}

fn substitute_journal_directory(root: &Path, displaced: &Path) {
    let journal = root.join(".cast/journal");
    fs::rename(&journal, displaced).unwrap();
    fs::create_dir(&journal).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
    create_private_lock(journal.join("state-transition.lock"));
}

fn substitute_journal_lock(root: &Path, displaced: &Path) {
    let lock = root.join(".cast/journal/state-transition.lock");
    fs::rename(&lock, displaced).unwrap();
    create_private_lock(lock);
}

fn create_private_lock(path: PathBuf) {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o600)).unwrap();
    file.sync_all().unwrap();
}

fn write_new_private_file(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    use std::io::Write as _;
    file.set_permissions(fs::Permissions::from_mode(0o600)).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
