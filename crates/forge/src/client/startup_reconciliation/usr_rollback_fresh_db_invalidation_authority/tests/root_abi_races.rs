use std::{
    fs,
    os::unix::fs::{MetadataExt as _, symlink},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationAdmission, arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture,
            arm_between_usr_rollback_fresh_db_invalidation_database_captures,
            fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::UsrRollbackFreshDbInvalidationEffectSeal,
    },
    db::state::arm_after_exact_fresh_transition_removal_attempt_before_reconciliation,
    transition_journal::RollbackActionOutcome,
};

use super::support::{CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout};

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
        Err(source) => panic!("inspect fresh-db invalidation root ABI: {source}"),
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
    fixture: &FreshDbInvalidationFixture,
    name: &'static str,
    target: &'static str,
    mutation: RootAbiMutation,
    label: String,
) -> impl FnOnce() + 'static {
    let root = fixture.fixture.fixture.installation.root.clone();
    let displaced_directory = tempfile::Builder::new()
        .prefix(".fresh-db-invalidation-root-abi-displaced-")
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

fn fixture_at_epoch(
    historical: bool,
    usr_outcome: RollbackActionOutcome,
    candidate_outcome: CandidateOutcome,
    row: FreshRowLayout,
) -> FreshDbInvalidationFixture {
    if historical {
        FreshDbInvalidationFixture::historical(
            CandidateSource::RootLinksComplete,
            usr_outcome,
            candidate_outcome,
            row,
        )
    } else {
        FreshDbInvalidationFixture::new(
            CandidateSource::RootLinksComplete,
            usr_outcome,
            candidate_outcome,
            row,
        )
    }
}

#[test]
fn startup_root_links_fresh_db_invalidation_capture_rejects_all_root_abi_mutations_across_fresh_outcomes() {
    let mut executions = 0;
    for historical in [false, true] {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for candidate_outcome in CandidateOutcome::ALL {
                for row in [FreshRowLayout::Present, FreshRowLayout::JointlyAbsent] {
                    for (name, target) in ROOT_ABI {
                        for mutation in RootAbiMutation::ALL {
                            let fixture = fixture_at_epoch(historical, usr_outcome, candidate_outcome, row);
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let canonical_before = fixture.canonical_bytes();
                            let root = fixture.fixture.fixture.installation.root.clone();
                            let root_abi_before = root_abi_snapshot(&root);
                            let case = format!(
                                "capture historical={historical} {usr_outcome:?} {candidate_outcome:?} {row:?} {name} {mutation:?}"
                            );
                            let hook = root_abi_mutation_hook(&fixture, name, target, mutation, case.clone());
                            arm_between_usr_rollback_fresh_db_invalidation_database_captures(hook);

                            assert!(matches!(
                                fixture.capture(&journal, &reservation).unwrap(),
                                UsrRollbackFreshDbInvalidationAdmission::Deferred
                            ));

                            assert_eq!(fixture.canonical_bytes(), canonical_before, "{case}");
                            match row {
                                FreshRowLayout::Present => fixture.assert_exact_present(),
                                FreshRowLayout::JointlyAbsent => fixture.assert_exact_joint_absence(),
                            }
                            assert_exact_root_abi_mutation(
                                &root_abi_before,
                                &root_abi_snapshot(&root),
                                name,
                                mutation,
                                &case,
                            );
                            executions += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(executions, 240);
}

#[test]
fn startup_root_links_fresh_db_invalidation_effect_revalidation_rejects_all_root_abi_mutations_before_removal() {
    let mut executions = 0;
    for historical in [false, true] {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for candidate_outcome in CandidateOutcome::ALL {
                for row in [FreshRowLayout::Present, FreshRowLayout::JointlyAbsent] {
                    for (name, target) in ROOT_ABI {
                        for mutation in RootAbiMutation::ALL {
                            let fixture = fixture_at_epoch(historical, usr_outcome, candidate_outcome, row);
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let canonical_before = fixture.canonical_bytes();
                            let root = fixture.fixture.fixture.installation.root.clone();
                            let root_abi_before = root_abi_snapshot(&root);
                            let case = format!(
                                "effect historical={historical} {usr_outcome:?} {candidate_outcome:?} {row:?} {name} {mutation:?}"
                            );
                            let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
                            let hook = root_abi_mutation_hook(&fixture, name, target, mutation, case.clone());
                            match row {
                                FreshRowLayout::Present => {
                                    let authority = fixture.capture_apply(&journal, &reservation);
                                    arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture(hook);
                                    assert!(authority.reconcile(&seal, &journal).is_err(), "{case}");
                                    fixture.assert_exact_present();
                                }
                                FreshRowLayout::JointlyAbsent => {
                                    let authority = fixture.capture_finish(&journal, &reservation);
                                    arm_before_usr_rollback_fresh_db_invalidation_fresh_namespace_capture(hook);
                                    assert!(authority.reconcile(&seal, &journal).is_err(), "{case}");
                                    fixture.assert_exact_joint_absence();
                                }
                            }

                            assert_eq!(fresh_db_invalidation_removal_call_count(), 0, "{case}");
                            assert_eq!(fixture.canonical_bytes(), canonical_before, "{case}");
                            assert_exact_root_abi_mutation(
                                &root_abi_before,
                                &root_abi_snapshot(&root),
                                name,
                                mutation,
                                &case,
                            );
                            executions += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(executions, 240);
}

#[test]
fn startup_root_links_fresh_db_invalidation_post_attempt_rejects_all_root_abi_mutations_after_one_removal() {
    let mut executions = 0;
    for historical in [false, true] {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for candidate_outcome in CandidateOutcome::ALL {
                for (name, target) in ROOT_ABI {
                    for mutation in RootAbiMutation::ALL {
                        let fixture = fixture_at_epoch(
                            historical,
                            usr_outcome,
                            candidate_outcome,
                            FreshRowLayout::Present,
                        );
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = fixture.capture_apply(&journal, &reservation);
                        let canonical_before = fixture.canonical_bytes();
                        let root = fixture.fixture.fixture.installation.root.clone();
                        let root_abi_before = root_abi_snapshot(&root);
                        let case = format!(
                            "post-attempt historical={historical} {usr_outcome:?} {candidate_outcome:?} {name} {mutation:?}"
                        );
                        let hook = root_abi_mutation_hook(&fixture, name, target, mutation, case.clone());
                        arm_after_exact_fresh_transition_removal_attempt_before_reconciliation(hook);
                        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();

                        assert!(authority.reconcile(&seal, &journal).is_err(), "{case}");

                        assert_eq!(fresh_db_invalidation_removal_call_count(), 1, "{case}");
                        fixture.assert_exact_joint_absence();
                        assert_eq!(fixture.canonical_bytes(), canonical_before, "{case}");
                        assert_exact_root_abi_mutation(
                            &root_abi_before,
                            &root_abi_snapshot(&root),
                            name,
                            mutation,
                            &case,
                        );
                        executions += 1;
                    }
                }
            }
        }
    }
    assert_eq!(executions, 120);
}
