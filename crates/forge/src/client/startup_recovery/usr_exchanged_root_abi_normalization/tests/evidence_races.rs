use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::client::{
    startup_gate,
    startup_reconciliation::{
        arm_after_usr_exchanged_root_abi_complete_sync, arm_after_usr_exchanged_root_abi_publication,
        arm_before_usr_exchanged_root_abi_complete_sync, arm_before_usr_exchanged_root_abi_publication,
        reset_usr_exchanged_root_abi_effect_counts, usr_exchanged_root_abi_complete_sync_attempts,
        usr_exchanged_root_abi_publication_attempts,
    },
};

use super::fixture::{Fixture, OperationKind, SourceCase, canonical_journal, create_private_directory};

#[test]
fn startup_usr_exchanged_root_abi_final_database_guard_blocks_pre_effect_race() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0);
    let source = fixture.canonical_bytes();
    let database = fixture.database.clone();
    let candidate = fixture.candidate_state;
    reset_usr_exchanged_root_abi_effect_counts();
    arm_before_usr_exchanged_root_abi_publication(move |_| {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });

    assert_execution_failure(fixture.enter());
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0);
    assert_eq!(fixture.canonical_bytes(), source);
    for (name, _) in super::fixture::ROOT_ABI {
        assert!(fs::symlink_metadata(fixture.installation.root.join(name)).is_err());
    }
}

#[test]
fn startup_usr_exchanged_root_abi_same_bytes_journal_replacement_breaks_record_binding() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0);
    let source = fixture.canonical_bytes();
    let journal_path = canonical_journal(&fixture.installation.root);
    let replacement_path = journal_path.clone();
    let replacement_bytes = source.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_before_usr_exchanged_root_abi_publication(move |_| {
        fs::remove_file(&replacement_path).unwrap();
        fs::write(&replacement_path, replacement_bytes).unwrap();
        fs::set_permissions(&replacement_path, fs::Permissions::from_mode(0o600)).unwrap();
    });

    assert_execution_failure(fixture.enter());
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0);
    assert_eq!(fixture.canonical_bytes(), source);
}

#[test]
fn startup_usr_exchanged_root_abi_public_journal_directory_replacement_blocks_effect() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0);
    let source = fixture.canonical_bytes();
    let canonical = canonical_journal(&fixture.installation.root);
    let journal = canonical.parent().unwrap().to_owned();
    let displaced = journal.with_extension("before-root-abi");
    let hook_displaced = displaced.clone();
    let replacement = journal.clone();
    let replacement_record = canonical.clone();
    let replacement_bytes = source.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_before_usr_exchanged_root_abi_publication(move |_| {
        fs::rename(&replacement, &hook_displaced).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
        let lock = replacement.join("state-transition.lock");
        fs::write(&lock, []).unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&replacement_record, replacement_bytes).unwrap();
        fs::set_permissions(&replacement_record, fs::Permissions::from_mode(0o600)).unwrap();
    });

    assert_execution_failure(fixture.enter());
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0);
    fs::remove_file(journal.join("state-transition")).unwrap();
    fs::remove_file(journal.join("state-transition.lock")).unwrap();
    fs::remove_dir(&journal).unwrap();
    fs::rename(displaced, &journal).unwrap();
    assert_eq!(fixture.canonical_bytes(), source);
}

#[test]
fn startup_usr_exchanged_root_abi_post_publication_database_race_fails_before_success() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0);
    let source = fixture.canonical_bytes();
    let database = fixture.database.clone();
    let candidate = fixture.candidate_state;
    reset_usr_exchanged_root_abi_effect_counts();
    arm_after_usr_exchanged_root_abi_publication(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });

    assert_execution_failure(fixture.enter());
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
    assert_eq!(fixture.canonical_bytes(), source);
    fixture.assert_complete_root_abi();
}

#[test]
fn startup_usr_exchanged_root_abi_complete_sync_guards_database_and_link_aba() {
    let database_race = Fixture::new(OperationKind::Archived, SourceCase::ExchangedPost);
    let source = database_race.canonical_bytes();
    let database = database_race.database.clone();
    let candidate = database_race.candidate_state;
    reset_usr_exchanged_root_abi_effect_counts();
    arm_before_usr_exchanged_root_abi_complete_sync(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });
    assert_execution_failure(database_race.enter());
    assert_eq!(database_race.canonical_bytes(), source);
    assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 0);

    let link_aba = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    let source = link_aba.canonical_bytes();
    let root = link_aba.installation.root.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_after_usr_exchanged_root_abi_complete_sync(move || {
        fs::remove_file(root.join("bin")).unwrap();
        std::os::unix::fs::symlink("usr/bin", root.join("bin")).unwrap();
    });
    assert_execution_failure(link_aba.enter());
    assert_eq!(link_aba.canonical_bytes(), source);
    assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 1);
}

#[test]
fn startup_usr_exchanged_root_abi_non_root_post_effect_race_is_ambiguous() {
    let fixture = Fixture::new(OperationKind::ActiveReblit, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0);
    let source = fixture.canonical_bytes();
    let inserted = fixture.installation.state_quarantine_dir().join("root-abi-race");
    reset_usr_exchanged_root_abi_effect_counts();
    arm_after_usr_exchanged_root_abi_publication(move || create_private_directory(&inserted));

    assert_execution_failure(fixture.enter());
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
    assert_eq!(fixture.canonical_bytes(), source);
    fixture.assert_complete_root_abi();
}

#[test]
fn startup_usr_exchanged_root_abi_root_file_and_symlink_races_are_ambiguous() {
    for symlink_entry in [false, true] {
        let fixture = Fixture::new(OperationKind::ActiveReblit, SourceCase::ExchangedPost);
        fixture.set_root_abi_subset(0);
        let source = fixture.canonical_bytes();
        let inserted = fixture.installation.root.join(if symlink_entry {
            "foreign-root-symlink"
        } else {
            "foreign-root-file"
        });
        reset_usr_exchanged_root_abi_effect_counts();
        arm_after_usr_exchanged_root_abi_publication(move || {
            if symlink_entry {
                std::os::unix::fs::symlink("usr/foreign", &inserted).unwrap();
            } else {
                fs::write(&inserted, b"foreign").unwrap();
            }
        });

        assert_execution_failure(fixture.enter());
        assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
        assert_eq!(fixture.canonical_bytes(), source);
        fixture.assert_complete_root_abi();
    }
}

#[test]
fn startup_usr_exchanged_root_abi_public_root_replacement_blocks_without_mutation() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0);
    let source = fixture.canonical_bytes();
    let root = fixture.installation.root.clone();
    let moved = root.with_extension("root-abi-retained");
    let hook_root = root.clone();
    let hook_moved = moved.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_before_usr_exchanged_root_abi_publication(move |_| {
        fs::rename(&hook_root, &hook_moved).unwrap();
        fs::create_dir(&hook_root).unwrap();
    });

    assert_execution_failure(fixture.enter());
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0);
    fs::remove_dir(&root).unwrap();
    fs::rename(&moved, &root).unwrap();
    assert_eq!(fixture.canonical_bytes(), source);
}

#[test]
fn startup_usr_exchanged_root_abi_post_publication_root_replacement_is_ambiguous() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    fixture.set_root_abi_subset(0);
    let source = fixture.canonical_bytes();
    let root = fixture.installation.root.clone();
    let moved = root.with_extension("root-abi-post-effect");
    let hook_root = root.clone();
    let hook_moved = moved.clone();
    reset_usr_exchanged_root_abi_effect_counts();
    arm_after_usr_exchanged_root_abi_publication(move || {
        fs::rename(&hook_root, &hook_moved).unwrap();
        fs::create_dir(&hook_root).unwrap();
    });

    assert_execution_failure(fixture.enter());
    assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
    fs::remove_dir(&root).unwrap();
    fs::rename(&moved, &root).unwrap();
    assert_eq!(fixture.canonical_bytes(), source);
    fixture.assert_complete_root_abi();
}

fn assert_execution_failure(error: startup_gate::Error) {
    assert!(matches!(
        error,
        startup_gate::Error::UsrExchangedRootAbiNormalizationExecution(_)
    ));
}
