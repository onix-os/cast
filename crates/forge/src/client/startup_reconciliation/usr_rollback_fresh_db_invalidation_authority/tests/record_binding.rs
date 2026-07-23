use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationAdmission,
            arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture,
            arm_between_usr_rollback_fresh_db_invalidation_database_captures,
            fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::UsrRollbackFreshDbInvalidationEffectSeal,
    },
    db::state::arm_after_exact_fresh_transition_removal_attempt_before_reconciliation,
    transition_journal::RollbackActionOutcome,
};

use super::support::{
    CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout, canonical_journal,
};

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_different_inode_hook(
    fixture: &FreshDbInvalidationFixture,
    label: &'static str,
) -> impl FnOnce() + 'static {
    let canonical = canonical_journal(&fixture.fixture.fixture.installation.root);
    let displaced = fixture
        .fixture
        .fixture
        .installation
        .root
        .join(".cast/journal")
        .join(format!(".{label}-displaced"));
    move || {
        let bytes = fs::read(&canonical).unwrap();
        fs::rename(&canonical, &displaced).unwrap();
        let retained_identity = inode_identity(&displaced);
        fs::write(&canonical, &bytes).unwrap();
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read(&canonical).unwrap(), bytes);
        assert_ne!(retained_identity, inode_identity(&canonical));
        fs::remove_file(displaced).unwrap();
    }
}

#[test]
fn startup_fresh_db_invalidation_capture_rejects_same_byte_source_inode_replacement() {
    for row in [FreshRowLayout::Present, FreshRowLayout::JointlyAbsent] {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::RootLinksComplete,
            RollbackActionOutcome::Applied,
            CandidateOutcome::AlreadySatisfied,
            row,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let canonical_before = fixture.canonical_bytes();
        arm_between_usr_rollback_fresh_db_invalidation_database_captures(
            same_byte_different_inode_hook(&fixture, "capture"),
        );

        assert!(matches!(
            fixture.capture(&journal, &reservation).unwrap(),
            UsrRollbackFreshDbInvalidationAdmission::Deferred
        ));

        assert_eq!(fixture.canonical_bytes(), canonical_before);
        match row {
            FreshRowLayout::Present => fixture.assert_exact_present(),
            FreshRowLayout::JointlyAbsent => fixture.assert_exact_joint_absence(),
        }
    }
}

#[test]
fn startup_fresh_db_invalidation_effect_rejects_same_byte_source_inode_replacement_before_removal() {
    for row in [FreshRowLayout::Present, FreshRowLayout::JointlyAbsent] {
        let fixture = FreshDbInvalidationFixture::new(
            CandidateSource::RootLinksComplete,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::Applied,
            row,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
        match row {
            FreshRowLayout::Present => {
                let authority = fixture.capture_apply(&journal, &reservation);
                arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture(
                    same_byte_different_inode_hook(&fixture, "effect-apply"),
                );
                assert!(authority.reconcile(&seal, &journal).is_err());
                fixture.assert_exact_present();
            }
            FreshRowLayout::JointlyAbsent => {
                let authority = fixture.capture_finish(&journal, &reservation);
                arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture(
                    same_byte_different_inode_hook(&fixture, "effect-finish"),
                );
                assert!(authority.reconcile(&seal, &journal).is_err());
                fixture.assert_exact_joint_absence();
            }
        }
        assert_eq!(fresh_db_invalidation_removal_call_count(), 0);
    }
}

#[test]
fn startup_fresh_db_invalidation_effect_rejects_same_byte_source_inode_replacement_after_one_removal() {
    let fixture = FreshDbInvalidationFixture::new(
        CandidateSource::RootLinksComplete,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
        FreshRowLayout::Present,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_apply(&journal, &reservation);
    arm_after_exact_fresh_transition_removal_attempt_before_reconciliation(
        same_byte_different_inode_hook(&fixture, "post-removal"),
    );
    let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();

    assert!(authority.reconcile(&seal, &journal).is_err());

    assert_eq!(fresh_db_invalidation_removal_call_count(), 1);
    fixture.assert_exact_joint_absence();
    assert_eq!(fixture.canonical_record(), fixture.record);
}
