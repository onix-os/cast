//! Database, provenance, journal, and namespace races at completion routing.

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, symlink},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate,
        startup_reconciliation::{
            RecoveryBlocker, UsrRollbackActiveReblitCompleteRouteAdmission,
            arm_before_usr_rollback_active_reblit_complete_route_fresh_namespace_capture,
            arm_between_usr_rollback_active_reblit_complete_route_database_captures,
        },
        startup_recovery::{
            UsrRollbackActiveReblitCompleteRoutePersistenceError,
            arm_before_usr_rollback_active_reblit_complete_route_final_revalidation,
            persist_usr_rollback_active_reblit_complete_route_and_reopen,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_complete_persistence_authority_error,
        assert_complete_route_journal_only, assert_exact_no_boot_completion_plan, assert_no_candidate_effects,
        build_active, capture_complete_route, capture_complete_route_ready, enter_candidate,
        expected_candidate_preserved, persist_candidate_preserved, reset_candidate_effect_observers,
        reset_complete_route_effect_observers,
    },
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

#[derive(Clone, Copy, Debug)]
enum RootAbiSeam {
    CaptureSandwich,
    FinalRevalidation,
}

impl RootAbiSeam {
    const ALL: [Self; 2] = [Self::CaptureSandwich, Self::FinalRevalidation];
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
        Err(source) => panic!("inspect ActiveReblit route root ABI: {source}"),
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
    fixture: &super::super::candidate_test_support::CandidatePreserveFixture,
    name: &'static str,
    target: &'static str,
    mutation: RootAbiMutation,
    label: String,
) -> impl FnOnce() + 'static {
    let root = fixture.fixture.installation.root.clone();
    let displaced_directory = tempfile::Builder::new()
        .prefix(".active-reblit-route-root-abi-displaced-")
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
                symlink(format!("usr/wrong-{name}"), &link).unwrap();
            }
            RootAbiMutation::SameTargetDifferentInode => {
                symlink(target, &link).unwrap();
            }
        }
        if !matches!(mutation, RootAbiMutation::Missing) {
            let selected_after = root_abi_link_identity(&root, name);
            assert_ne!(
                (selected_after.device, selected_after.inode),
                (selected_before.device, selected_before.inode),
                "{label}"
            );
        }
        fs::remove_file(displaced).unwrap();
    }
}

#[test]
fn startup_active_reblit_complete_route_root_links_rejects_all_root_abi_mutations_at_two_seams() {
    let mut cases = 0;
    for seam in RootAbiSeam::ALL {
        for epoch in Epoch::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOrigin::ALL {
                    for (name, target) in ROOT_ABI {
                        for mutation in RootAbiMutation::ALL {
                            let fixture = build_active(
                                epoch,
                                CandidateSource::RootLinksComplete,
                                usr_outcome,
                                CandidateOrigin::AlreadySatisfied,
                            );
                            let source = persist_candidate_preserved(&fixture, candidate_outcome);
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let canonical_before = fs::read(
                                fixture.fixture.installation.root.join(".cast/journal/state-transition"),
                            )
                            .unwrap();
                            let database_before = fixture.fixture.database_snapshot();
                            let root = fixture.fixture.installation.root.clone();
                            let root_abi_before = root_abi_snapshot(&root);
                            let case = format!(
                                "{seam:?} {epoch:?} {usr_outcome:?} {candidate_outcome:?} {name} {mutation:?}"
                            );
                            assert_exact_no_boot_completion_plan(
                                &source,
                                CandidateSource::RootLinksComplete,
                            );
                            reset_complete_route_effect_observers();
                            let hook = root_abi_mutation_hook(
                                &fixture,
                                name,
                                target,
                                mutation,
                                case.clone(),
                            );

                            match seam {
                                RootAbiSeam::CaptureSandwich => {
                                    arm_between_usr_rollback_active_reblit_complete_route_database_captures(hook);
                                    assert!(matches!(
                                        capture_complete_route(
                                            &fixture,
                                            &journal,
                                            &reservation,
                                            &source,
                                        )
                                        .unwrap(),
                                        UsrRollbackActiveReblitCompleteRouteAdmission::Deferred
                                    ));
                                }
                                RootAbiSeam::FinalRevalidation => {
                                    let authority = capture_complete_route_ready(
                                        &fixture,
                                        &journal,
                                        &reservation,
                                        &source,
                                    );
                                    arm_before_usr_rollback_active_reblit_complete_route_final_revalidation(hook);
                                    let error =
                                        persist_usr_rollback_active_reblit_complete_route_and_reopen(
                                            journal, authority,
                                        )
                                        .unwrap_err();
                                    assert!(matches!(
                                        error,
                                        UsrRollbackActiveReblitCompleteRoutePersistenceError::Authority(_)
                                    ));
                                }
                            }

                            assert_eq!(
                                fs::read(
                                    fixture
                                        .fixture
                                        .installation
                                        .root
                                        .join(".cast/journal/state-transition"),
                                )
                                .unwrap(),
                                canonical_before,
                                "{case}"
                            );
                            assert_eq!(fixture.fixture.canonical_record(), source, "{case}");
                            assert_eq!(fixture.fixture.database_snapshot(), database_before, "{case}");
                            assert!(active_wrapper_path(&fixture).join("usr").is_dir(), "{case}");
                            assert_complete_route_journal_only();
                            assert_exact_root_abi_mutation(
                                &root_abi_before,
                                &root_abi_snapshot(&root),
                                name,
                                mutation,
                                &case,
                            );
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 240);
}

#[test]
fn startup_active_reblit_complete_route_rejects_database_provenance_journal_and_namespace_races() {
    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    reset_candidate_effect_observers();
    arm_between_usr_rollback_active_reblit_complete_route_database_captures(move || {
        database.remove(&candidate).unwrap();
    });

    let database_error = enter_candidate(&fixture);

    assert_pending_blocker(&database_error, RecoveryBlocker::DatabaseConflict);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert!(fixture.fixture.database.get(candidate).is_err());
    assert_no_candidate_effects();

    let fixture = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
    let namespace_before = fixture.fixture.namespace_snapshot();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    reset_candidate_effect_observers();
    arm_between_usr_rollback_active_reblit_complete_route_database_captures(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });

    let provenance_error = enter_candidate(&fixture);

    assert_pending_blocker(&provenance_error, RecoveryBlocker::MetadataProvenanceConflict);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert!(
        fixture
            .fixture
            .database
            .metadata_provenance(candidate)
            .unwrap()
            .is_none()
    );
    assert_no_candidate_effects();

    let fixture = build_active(
        Epoch::Current,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::AlreadySatisfied);
    let changed = expected_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_complete_route_final_revalidation(fixture.journal_change_hook());

    let journal_error = enter_candidate(&fixture);

    assert_complete_persistence_authority_error(&journal_error);
    assert_ne!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.canonical_record(), changed);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    let fixture = build_active(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOrigin::AlreadySatisfied,
    );
    let source = persist_candidate_preserved(&fixture, CandidateOrigin::Applied);
    let database_before = fixture.fixture.database_snapshot();
    reset_candidate_effect_observers();
    arm_before_usr_rollback_active_reblit_complete_route_fresh_namespace_capture(
        fixture.namespace_change_hook("active-reblit-complete-route-race".to_owned()),
    );

    let namespace_error = enter_candidate(&fixture);

    assert_complete_persistence_authority_error(&namespace_error);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert!(
        fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("active-reblit-complete-route-race")
            .is_dir()
    );
    assert!(active_wrapper_path(&fixture).join("usr").is_dir());
    assert_no_candidate_effects();
}

fn assert_pending_blocker(error: &startup_gate::Error, blocker: RecoveryBlocker) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected recovery-pending blocker {blocker:?}, got {error:?}");
    };
    assert!(
        pending.blockers().contains(&blocker),
        "expected blocker {blocker:?}, got {:?}",
        pending.blockers()
    );
}
