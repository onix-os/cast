//! Exact NewState terminal journal inode admission contracts.

use std::{
    fs,
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFinalizationAdmission,
            arm_between_usr_rollback_finalization_database_captures,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source};

#[test]
fn startup_usr_rollback_finalization_binding_rejects_same_bytes_on_a_different_inode() {
    for during_capture in [true, false] {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::Applied,
            Source::RootLinksComplete,
            RollbackActionOutcome::Applied,
            CandidateResult::AlreadySatisfied,
        );
        assert_eq!(fixture.record.generation, 18);
        let installation = &fixture.fixture.fixture.fixture.installation;
        let canonical = installation.root.join(".cast/journal/state-transition");
        let displaced = installation
            .state_quarantine_dir()
            .join("new-state-finalization-original-inode");
        let bytes = fs::read(&canonical).unwrap();
        let hook_canonical = canonical.clone();
        let hook_displaced = displaced.clone();
        let hook_bytes = bytes.clone();
        let replace = move || replace_with_same_bytes(&hook_canonical, &hook_displaced, &hook_bytes);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();

        if during_capture {
            arm_between_usr_rollback_finalization_database_captures(replace);
            let admission = fixture.capture(&journal, &reservation).unwrap();
            assert!(matches!(admission, UsrRollbackFinalizationAdmission::Deferred));
        } else {
            let authority = fixture.capture_ready(&journal, &reservation);
            replace();
            let error = authority.revalidate(&journal).unwrap_err();
            assert_eq!(
                error.to_string(),
                "the exact retained NewState terminal journal inode no longer matches its captured binding"
            );
        }
        assert_eq!(fs::read(&canonical).unwrap(), bytes);
        assert_eq!(fs::read(&displaced).unwrap(), bytes);
        fixture.fixture.assert_exact_joint_absence();
    }
}

fn replace_with_same_bytes(canonical: &Path, displaced: &Path, bytes: &[u8]) {
    fs::rename(canonical, displaced).unwrap();
    let mut replacement = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(canonical)
        .unwrap();
    replacement
        .set_permissions(fs::Permissions::from_mode(0o600))
        .unwrap();
    replacement.write_all(bytes).unwrap();
    replacement.sync_all().unwrap();
}
