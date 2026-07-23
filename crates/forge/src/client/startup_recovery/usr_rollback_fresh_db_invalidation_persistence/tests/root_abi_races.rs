use std::{
    fs,
    os::unix::fs::{MetadataExt as _, symlink},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture,
            fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::{
            UsrRollbackFreshDbInvalidationPersistenceError,
            arm_before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation,
            persist_usr_rollback_fresh_db_invalidation_and_reopen,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::support::{
    CandidateResult, Fixture, FreshDbInvalidationOrigin, Source, database_snapshot, effect_authority,
    fixture_for_origin,
};

const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

#[derive(Clone, Copy, Debug)]
enum RootAbiMutation {
    Missing,
    WrongTarget,
    SameTargetDifferentInode,
}

impl RootAbiMutation {
    const ALL: [Self; 3] = [Self::Missing, Self::WrongTarget, Self::SameTargetDifferentInode];
}

#[derive(Debug, Eq, PartialEq)]
struct RootAbiLinkIdentity {
    target: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
}

fn root_abi_link_identity(root: &Path, name: &str) -> RootAbiLinkIdentity {
    let link = root.join(name);
    let metadata = fs::symlink_metadata(&link).unwrap();
    assert!(metadata.file_type().is_symlink());
    RootAbiLinkIdentity {
        target: fs::read_link(link).unwrap(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        links: metadata.nlink(),
    }
}

fn root_abi_snapshot(root: &Path) -> [Option<RootAbiLinkIdentity>; 5] {
    ROOT_ABI.map(|(name, _)| match fs::symlink_metadata(root.join(name)) {
        Ok(_) => Some(root_abi_link_identity(root, name)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => panic!("inspect fresh-invalidation root ABI: {source}"),
    })
}

fn assert_exact_root_abi_mutation(
    before: &[Option<RootAbiLinkIdentity>; 5],
    after: &[Option<RootAbiLinkIdentity>; 5],
    selected_name: &str,
    mutation: RootAbiMutation,
    label: &str,
) {
    let selected_index = ROOT_ABI
        .iter()
        .position(|(name, _)| *name == selected_name)
        .unwrap();
    for (index, (_, expected_target)) in ROOT_ABI.into_iter().enumerate() {
        let original = before[index]
            .as_ref()
            .unwrap_or_else(|| panic!("root ABI fixture link {index} was missing before {label}"));
        assert_eq!(original.target, PathBuf::from(expected_target), "{label}");
        if index != selected_index {
            assert_eq!(after[index].as_ref(), Some(original), "{label}");
        }
    }

    let original = before[selected_index].as_ref().unwrap();
    match mutation {
        RootAbiMutation::Missing => assert!(after[selected_index].is_none(), "{label}"),
        RootAbiMutation::WrongTarget => {
            let changed = after[selected_index].as_ref().unwrap();
            assert_eq!(changed.target, PathBuf::from(format!("usr/wrong-{selected_name}")), "{label}");
            assert_eq!(changed.device, original.device, "{label}");
            assert_ne!(changed.inode, original.inode, "{label}");
            assert_eq!(changed.mode, original.mode, "{label}");
            assert_eq!(changed.links, original.links, "{label}");
        }
        RootAbiMutation::SameTargetDifferentInode => {
            let changed = after[selected_index].as_ref().unwrap();
            assert_eq!(changed.target, original.target, "{label}");
            assert_eq!(changed.device, original.device, "{label}");
            assert_ne!(changed.inode, original.inode, "{label}");
            assert_eq!(changed.mode, original.mode, "{label}");
            assert_eq!(changed.links, original.links, "{label}");
        }
    }
}

fn root_abi_mutation_hook(
    fixture: &Fixture,
    name: &'static str,
    target: &'static str,
    mutation: RootAbiMutation,
    label: String,
) -> impl FnOnce() + 'static {
    let root = fixture.fixture.fixture.installation.root.clone();
    let displaced_directory = tempfile::Builder::new()
        .prefix(".fresh-invalidation-root-abi-displaced-")
        .tempdir_in(root.parent().unwrap())
        .unwrap();
    assert!(!displaced_directory.path().starts_with(&root));
    move || {
        let link = root.join(name);
        let displaced = displaced_directory.path().join(name);
        let selected_before = root_abi_link_identity(&root, name);
        assert_eq!(selected_before.target, Path::new(target), "{label}");
        fs::rename(&link, &displaced).unwrap();
        match mutation {
            RootAbiMutation::Missing => {
                let error = fs::symlink_metadata(&link).unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{label}");
            }
            RootAbiMutation::WrongTarget => {
                let wrong_target = PathBuf::from(format!("usr/wrong-{name}"));
                symlink(&wrong_target, &link).unwrap();
                let selected_after = root_abi_link_identity(&root, name);
                assert_ne!(
                    (selected_after.device, selected_after.inode),
                    (selected_before.device, selected_before.inode),
                    "{label}"
                );
            }
            RootAbiMutation::SameTargetDifferentInode => {
                symlink(target, &link).unwrap();
                let selected_after = root_abi_link_identity(&root, name);
                assert_ne!(
                    (selected_after.device, selected_after.inode),
                    (selected_before.device, selected_before.inode),
                    "{label}"
                );
            }
        }
        fs::remove_file(displaced).unwrap();
    }
}

#[test]
fn startup_root_links_fresh_db_invalidation_initial_persistence_revalidation_rejects_all_root_abi_mutations() {
    let mut executions = 0;
    for historical in [false, true] {
        for origin in FreshDbInvalidationOrigin::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateResult::ALL {
                    for (name, target) in ROOT_ABI {
                        for mutation in RootAbiMutation::ALL {
                            executions += 1;
                            let fixture = fixture_for_origin(
                                origin,
                                historical,
                                Source::RootLinksComplete,
                                usr_outcome,
                                candidate_outcome,
                            );
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let authority = effect_authority(&fixture, &journal, &reservation, origin);
                            let canonical_before = fixture.canonical_bytes();
                            let database_before = database_snapshot(&fixture);
                            let expected_removals = usize::from(origin == FreshDbInvalidationOrigin::Applied);
                            let root = fixture.fixture.fixture.installation.root.clone();
                            let root_abi_before = root_abi_snapshot(&root);
                            let case = format!(
                                "initial {origin:?} historical={historical} {usr_outcome:?} {candidate_outcome:?} {name} {mutation:?}"
                            );
                            let hook = root_abi_mutation_hook(&fixture, name, target, mutation, case.clone());
                            hook();

                            let error =
                                persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)
                                    .unwrap_err();

                            assert!(matches!(
                                error,
                                UsrRollbackFreshDbInvalidationPersistenceError::Authority(_)
                            ));
                            assert_eq!(fixture.canonical_bytes(), canonical_before);
                            assert_eq!(database_snapshot(&fixture), database_before);
                            fixture.assert_exact_joint_absence();
                            assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
                            assert_exact_root_abi_mutation(
                                &root_abi_before,
                                &root_abi_snapshot(&root),
                                name,
                                mutation,
                                &case,
                            );
                        }
                    }
                }
            }
        }
    }
    assert_eq!(executions, 240);
}

#[test]
fn startup_root_links_fresh_db_invalidation_final_persistence_revalidation_rejects_all_root_abi_mutations() {
    let mut executions = 0;
    for historical in [false, true] {
        for origin in FreshDbInvalidationOrigin::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateResult::ALL {
                    for (name, target) in ROOT_ABI {
                        for mutation in RootAbiMutation::ALL {
                            executions += 1;
                            let fixture = fixture_for_origin(
                                origin,
                                historical,
                                Source::RootLinksComplete,
                                usr_outcome,
                                candidate_outcome,
                            );
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let authority = effect_authority(&fixture, &journal, &reservation, origin);
                            let canonical_before = fixture.canonical_bytes();
                            let database_before = database_snapshot(&fixture);
                            let expected_removals = usize::from(origin == FreshDbInvalidationOrigin::Applied);
                            let root = fixture.fixture.fixture.installation.root.clone();
                            let root_abi_before = root_abi_snapshot(&root);
                            let case = format!(
                                "final {origin:?} historical={historical} {usr_outcome:?} {candidate_outcome:?} {name} {mutation:?}"
                            );
                            let hook = root_abi_mutation_hook(&fixture, name, target, mutation, case.clone());
                            arm_before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation(move || {
                                arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture(hook);
                            });

                            let error =
                                persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)
                                    .unwrap_err();

                            assert!(matches!(
                                error,
                                UsrRollbackFreshDbInvalidationPersistenceError::Authority(_)
                            ));
                            assert_eq!(fixture.canonical_bytes(), canonical_before);
                            assert_eq!(database_snapshot(&fixture), database_before);
                            fixture.assert_exact_joint_absence();
                            assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
                            assert_exact_root_abi_mutation(
                                &root_abi_before,
                                &root_abi_snapshot(&root),
                                name,
                                mutation,
                                &case,
                            );
                        }
                    }
                }
            }
        }
    }
    assert_eq!(executions, 240);
}
