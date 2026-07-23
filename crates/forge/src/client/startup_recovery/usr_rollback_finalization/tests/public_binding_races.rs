//! Adversarial public journal directory, lock, and entry-set substitutions.

use std::{
    fs,
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::arm_before_usr_rollback_finalization_fresh_namespace_capture,
        startup_recovery::{
            UsrRollbackFinalizationError, arm_after_usr_rollback_finalization_delete,
            arm_before_usr_rollback_finalization_final_revalidation, finalize_usr_rollback,
        },
    },
    transition_journal::{
        PublicBindingRevalidationBoundary, RollbackActionOutcome, StorageError,
        TransitionJournalRecordDeleteError, arm_public_binding_revalidation_callback,
        assert_public_binding_revalidation_callback_consumed, encode,
    },
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source};

#[derive(Clone, Copy, Debug)]
enum Timing {
    BeforeDelete,
    AfterDelete,
}

#[test]
fn startup_usr_rollback_finalization_rejects_public_journal_directory_and_lock_substitution() {
    for timing in [Timing::BeforeDelete, Timing::AfterDelete] {
        for directory in [true, false] {
            let fixture = exact_fixture();
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = fixture.capture_ready(&journal, &reservation);
            let root = fixture.installation().root.clone();
            let displaced = root.join(if directory {
                ".cast/journal-displaced-by-new-state-finalization-test"
            } else {
                ".cast/journal-lock-displaced-by-new-state-finalization-test"
            });
            let hook_root = root.clone();
            let hook_displaced = displaced.clone();
            arm_at(timing, move || {
                if directory {
                    substitute_journal_directory(&hook_root, &hook_displaced);
                } else {
                    substitute_journal_lock(&hook_root, &hook_displaced);
                }
            });

            let error = finalize_usr_rollback(journal, authority).unwrap_err();

            assert_binding_error(timing, &error);
            assert!(displaced.exists());
            fixture.assert_no_second_removal();
        }
    }
}

#[test]
fn startup_usr_rollback_finalization_rejects_hidden_canonical_record_displacement() {
    for timing in [Timing::BeforeDelete, Timing::AfterDelete] {
        let fixture = exact_fixture();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let canonical = fixture.installation().root.join(".cast/journal/state-transition");
        let displaced = fixture
            .installation()
            .state_quarantine_dir()
            .join("state-transition-displaced-by-new-state-finalization-test");
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

        assert_binding_error(timing, &error);
        assert_eq!(fs::read(&displaced).unwrap(), exact_bytes);
        fixture.assert_no_second_removal();
    }
}

#[test]
fn startup_usr_rollback_finalization_bound_delete_never_unlinks_a_last_seam_replacement() {
    let fixture = exact_fixture();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let root = &fixture.installation().root;
    let canonical = root.join(".cast/journal/state-transition");
    let displaced = fixture
        .installation()
        .state_quarantine_dir()
        .join("new-state-finalization-retained-original");
    let exact_bytes = fs::read(&canonical).unwrap();
    let hook_canonical = canonical.clone();
    let hook_displaced = displaced.clone();
    let hook_bytes = exact_bytes.clone();
    arm_public_binding_revalidation_callback(
        PublicBindingRevalidationBoundary::BeforeBoundDeleteDetach,
        move || {
            fs::rename(&hook_canonical, &hook_displaced).unwrap();
            write_new_private_file(&hook_canonical, &hook_bytes);
        },
    );

    let error = finalize_usr_rollback(journal, authority).unwrap_err();

    assert_public_binding_revalidation_callback_consumed();
    assert!(matches!(
        error,
        UsrRollbackFinalizationError::Delete(TransitionJournalRecordDeleteError::Detached(
            StorageError::CanonicalChanged
        ))
    ));
    assert_eq!(fs::read(displaced).unwrap(), exact_bytes);
    assert!(!canonical.exists());
    let retained_replacements = fs::read_dir(root.join(".cast/journal"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".state-transition.delete-")
        })
        .map(|entry| fs::read(entry.path()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(retained_replacements, vec![exact_bytes]);
    fixture.assert_no_second_removal();
}

#[test]
fn startup_usr_rollback_finalization_rejects_source_recreation_after_delete_and_absence_proof() {
    for inside_absence_proof in [false, true] {
        let fixture = exact_fixture();
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let canonical = fixture.installation().root.join(".cast/journal/state-transition");
        let bytes = encode(&fixture.source).unwrap();
        arm_after_usr_rollback_finalization_delete(move || {
            if inside_absence_proof {
                arm_before_usr_rollback_finalization_fresh_namespace_capture(move || {
                    write_new_private_file(&canonical, &bytes)
                });
            } else {
                write_new_private_file(&canonical, &bytes);
            }
        });

        let error = finalize_usr_rollback(journal, authority).unwrap_err();

        assert!(matches!(error, UsrRollbackFinalizationError::PostDeleteAuthority(_)));
        assert_eq!(fixture.canonical_record(), fixture.source);
        fixture.assert_no_second_removal();
    }
}

fn exact_fixture() -> FinalizationFixture {
    FinalizationFixture::new(
        FreshDbOutcome::Applied,
        Source::RootLinksComplete,
        RollbackActionOutcome::Applied,
        CandidateResult::AlreadySatisfied,
    )
}

fn arm_at(timing: Timing, hook: impl FnOnce() + 'static) {
    match timing {
        Timing::BeforeDelete => arm_before_usr_rollback_finalization_final_revalidation(hook),
        Timing::AfterDelete => arm_after_usr_rollback_finalization_delete(hook),
    }
}

fn assert_binding_error(timing: Timing, error: &UsrRollbackFinalizationError) {
    let matched = matches!(
        (timing, error),
        (Timing::BeforeDelete, UsrRollbackFinalizationError::Authority(_))
            | (Timing::AfterDelete, UsrRollbackFinalizationError::PostDeleteAuthority(_))
    );
    assert!(matched, "timing={timing:?}: {error:?}");
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
    file.set_permissions(fs::Permissions::from_mode(0o600)).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
