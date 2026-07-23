use std::{fs, os::unix::fs::symlink};

use crate::{
    client::{
        arm_before_retained_root_abi_link_publication, arm_retained_root_abi_sync_fault, startup_gate,
        startup_reconciliation::{
            arm_after_usr_exchanged_root_abi_publication, arm_usr_exchanged_root_abi_complete_sync_fault,
            reset_usr_exchanged_root_abi_effect_counts, usr_exchanged_root_abi_complete_sync_attempts,
            usr_exchanged_root_abi_publication_attempts,
        },
    },
    transition_journal::Phase,
};

use super::fixture::{Fixture, OperationKind, SourceCase, pending};

#[test]
fn startup_usr_exchanged_root_abi_publisher_sync_failure_requires_complete_retry_sync() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0);
    let source = fixture.canonical_bytes();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_retained_root_abi_sync_fault();

    assert_execution_failure(fixture.enter());
    fixture.assert_complete_root_abi();
    assert_eq!(fixture.canonical_bytes(), source);
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
    assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 0);

    assert_eq!(pending(&fixture.enter()).phase(), Phase::RollbackDecided);
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
    assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 1);
}

#[test]
fn startup_usr_exchanged_root_abi_complete_sync_failure_retries_without_publication() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::ExchangedPost);
    let source = fixture.canonical_bytes();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_usr_exchanged_root_abi_complete_sync_fault();

    assert_execution_failure(fixture.enter());
    assert_eq!(fixture.canonical_bytes(), source);
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0);
    assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 1);

    assert_eq!(pending(&fixture.enter()).phase(), Phase::RollbackDecided);
    assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 2);
}

#[test]
fn startup_usr_exchanged_root_abi_exact_eexist_is_authenticated_and_wrong_eexist_fails_partial() {
    let exact = Fixture::new(OperationKind::Archived, SourceCase::ExchangedPost);
    exact.set_root_abi_subset(0);
    let exact_root = exact.installation.root.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_before_retained_root_abi_link_publication(2, move || {
        symlink("usr/lib", exact_root.join("lib")).unwrap();
    });
    assert_eq!(pending(&exact.enter()).phase(), Phase::UsrExchanged);
    exact.assert_complete_root_abi();
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);

    let wrong = Fixture::new(OperationKind::Archived, SourceCase::ExchangedPost);
    wrong.set_root_abi_subset(0);
    let source = wrong.canonical_bytes();
    let wrong_root = wrong.installation.root.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_before_retained_root_abi_link_publication(2, move || {
        symlink("usr/not-lib", wrong_root.join("lib")).unwrap();
    });
    assert_execution_failure(wrong.enter());
    assert_eq!(fs::read_link(wrong.installation.root.join("lib")).unwrap(), std::path::Path::new("usr/not-lib"));
    assert!(fs::symlink_metadata(wrong.installation.root.join("bin")).is_ok());
    assert!(fs::symlink_metadata(wrong.installation.root.join("sbin")).is_ok());
    assert!(fs::symlink_metadata(wrong.installation.root.join("lib32")).is_err());
    assert!(fs::symlink_metadata(wrong.installation.root.join("lib64")).is_err());
    assert_eq!(wrong.canonical_bytes(), source);
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
}

#[test]
fn startup_usr_exchanged_root_abi_foreign_collision_at_every_publication_index_stays_at_source() {
    const PUBLICATION_ORDER: [(&str, &str); 5] = [
        ("sbin", "usr/sbin"),
        ("bin", "usr/bin"),
        ("lib", "usr/lib"),
        ("lib64", "usr/lib"),
        ("lib32", "usr/lib32"),
    ];

    for collision in 0..PUBLICATION_ORDER.len() {
        let fixture = Fixture::new(OperationKind::Archived, SourceCase::ExchangedPost);
        fixture.set_root_abi_subset(0);
        let source = fixture.canonical_bytes();
        let root = fixture.installation.root.clone();
        let collision_name = PUBLICATION_ORDER[collision].0;
        reset_usr_exchanged_root_abi_effect_counts();
        arm_before_retained_root_abi_link_publication(collision, move || {
            symlink("usr/foreign", root.join(collision_name)).unwrap();
        });

        assert_execution_failure(fixture.enter());
        assert_eq!(fixture.canonical_bytes(), source, "collision index {collision}");
        assert_eq!(fixture.canonical_record().phase, Phase::UsrExchanged);
        assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
        for (index, (name, target)) in PUBLICATION_ORDER.into_iter().enumerate() {
            let path = fixture.installation.root.join(name);
            if index < collision {
                assert_eq!(fs::read_link(path).unwrap(), std::path::Path::new(target));
            } else if index == collision {
                assert_eq!(fs::read_link(path).unwrap(), std::path::Path::new("usr/foreign"));
            } else {
                assert!(fs::symlink_metadata(path).is_err());
            }
        }
    }
}

#[test]
fn startup_usr_exchanged_root_abi_next_name_race_after_preflight_fails_partial() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0);
    let source = fixture.canonical_bytes();
    let root = fixture.installation.root.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_before_retained_root_abi_link_publication(2, move || {
        symlink("usr/lib", root.join("lib.next")).unwrap();
    });

    assert_execution_failure(fixture.enter());
    assert_eq!(fixture.canonical_bytes(), source);
    assert_eq!(fixture.canonical_record().phase, Phase::UsrExchanged);
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
    assert_eq!(fs::read_link(fixture.installation.root.join("lib.next")).unwrap(), std::path::Path::new("usr/lib"));
}

#[test]
fn startup_usr_exchanged_root_abi_existing_and_new_exact_target_aba_fail_closed() {
    let existing = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    existing.set_root_abi_subset(1);
    let source = existing.canonical_bytes();
    let root = existing.installation.root.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_before_retained_root_abi_link_publication(1, move || {
        fs::remove_file(root.join("bin")).unwrap();
        symlink("usr/bin", root.join("bin")).unwrap();
    });
    assert_execution_failure(existing.enter());
    assert_eq!(existing.canonical_bytes(), source);

    let created = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    created.set_root_abi_subset(0);
    let source = created.canonical_bytes();
    let root = created.installation.root.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_after_usr_exchanged_root_abi_publication(move || {
        fs::remove_file(root.join("bin")).unwrap();
        symlink("usr/bin", root.join("bin")).unwrap();
    });
    assert_execution_failure(created.enter());
    assert_eq!(created.canonical_bytes(), source);
    created.assert_complete_root_abi();
}

fn assert_execution_failure(error: startup_gate::Error) {
    assert!(matches!(
        error,
        startup_gate::Error::UsrExchangedRootAbiNormalizationExecution(_)
    ));
}
