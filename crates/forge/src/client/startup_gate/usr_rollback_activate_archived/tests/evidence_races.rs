//! Database, provenance, journal, and namespace race boundaries.

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
            RecoveryBlocker, arm_before_usr_rollback_activate_archived_complete_route_fresh_namespace_capture,
            arm_between_usr_rollback_activate_archived_complete_route_database_captures,
        },
        startup_recovery::{
            UsrRollbackActivateArchivedCompleteRoutePersistenceError,
            arm_before_usr_rollback_activate_archived_complete_route_final_revalidation,
            persist_usr_rollback_activate_archived_complete_route_and_reopen,
        },
    },
    transition_journal::{Phase, RollbackActionOutcome, encode},
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
    FreshNamespaceCapture,
}

impl RootAbiSeam {
    const ALL: [Self; 3] = [Self::CaptureSandwich, Self::FinalRevalidation, Self::FreshNamespaceCapture];
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
        Err(source) => panic!("inspect ActivateArchived route root ABI: {source}"),
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
    fixture: &RouteFixture,
    name: &'static str,
    target: &'static str,
    mutation: RootAbiMutation,
    label: String,
) -> impl FnOnce() + 'static {
    let root = fixture.fixture.fixture.installation.root.clone();
    let displaced_directory = tempfile::Builder::new()
        .prefix(".activate-archived-route-root-abi-displaced-")
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
fn startup_activate_archived_complete_route_root_links_rejects_all_root_abi_mutations_at_three_seams() {
    let mut cases = 0;
    for seam in RootAbiSeam::ALL {
        for epoch in Epoch::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    for (name, target) in ROOT_ABI {
                        for mutation in RootAbiMutation::ALL {
                            let fixture = RouteFixture::new(
                                epoch,
                                CandidateSource::RootLinksComplete,
                                usr_outcome,
                                candidate_outcome,
                            );
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let canonical_before = fixture.canonical_bytes();
                            let database_before = fixture.database_snapshot();
                            let root = fixture.fixture.fixture.installation.root.clone();
                            let root_abi_before = root_abi_snapshot(&root);
                            let case = format!(
                                "{seam:?} {epoch:?} {usr_outcome:?} {candidate_outcome:?} {name} {mutation:?}"
                            );
                            let hook = root_abi_mutation_hook(&fixture, name, target, mutation, case.clone());

                            match seam {
                                RootAbiSeam::CaptureSandwich => {
                                    arm_between_usr_rollback_activate_archived_complete_route_database_captures(hook);
                                    assert!(matches!(
                                        fixture.capture(&journal, &reservation).unwrap(),
                                        crate::client::startup_reconciliation::UsrRollbackActivateArchivedCompleteRouteAdmission::Deferred
                                    ));
                                }
                                RootAbiSeam::FinalRevalidation => {
                                    let authority = fixture.capture_ready(&journal, &reservation);
                                    arm_before_usr_rollback_activate_archived_complete_route_final_revalidation(hook);
                                    let error = persist_usr_rollback_activate_archived_complete_route_and_reopen(
                                        journal, authority,
                                    )
                                    .unwrap_err();
                                    assert!(matches!(
                                        error,
                                        UsrRollbackActivateArchivedCompleteRoutePersistenceError::Authority(_)
                                    ));
                                }
                                RootAbiSeam::FreshNamespaceCapture => {
                                    let authority = fixture.capture_ready(&journal, &reservation);
                                    arm_before_usr_rollback_activate_archived_complete_route_final_revalidation(
                                        move || {
                                            arm_before_usr_rollback_activate_archived_complete_route_fresh_namespace_capture(
                                                hook,
                                            );
                                        },
                                    );
                                    let error = persist_usr_rollback_activate_archived_complete_route_and_reopen(
                                        journal, authority,
                                    )
                                    .unwrap_err();
                                    assert!(matches!(
                                        error,
                                        UsrRollbackActivateArchivedCompleteRoutePersistenceError::Authority(_)
                                    ));
                                }
                            }

                            assert_eq!(fixture.canonical_bytes(), canonical_before, "{case}");
                            assert_eq!(fixture.database_snapshot(), database_before, "{case}");
                            assert_eq!(candidate_move_count(), 0, "{case}");
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
    assert_eq!(cases, 360);
}

use super::support::{
    CandidateOutcome, CandidateSource, Epoch, RouteFixture, assert_complete_persistence_authority_error,
    candidate_move_count, enter_route, reset_candidate_observers,
};

#[derive(Clone, Copy, Debug)]
enum CaptureRace {
    Database,
    Provenance,
    Namespace,
}

#[test]
fn startup_activate_archived_complete_route_capture_sandwich_rejects_database_provenance_and_namespace_races() {
    for race in [CaptureRace::Database, CaptureRace::Provenance, CaptureRace::Namespace] {
        let fixture = exact_fixture();
        let canonical_before = fixture.canonical_bytes();
        let rows_before = fixture.fixture.fixture.database.all().unwrap();
        let namespace_before = fixture.namespace_snapshot();
        let inserted = fixture
            .fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("activate-archived-capture-race");
        let hook: Box<dyn FnOnce()> = match race {
            CaptureRace::Database => {
                let database = fixture.fixture.fixture.database.clone();
                let candidate = fixture.fixture.fixture.candidate_state;
                Box::new(move || database.remove(&candidate).unwrap())
            }
            CaptureRace::Provenance => {
                let database = fixture.fixture.fixture.database.clone();
                let candidate = fixture.fixture.fixture.candidate_state;
                Box::new(move || database.delete_metadata_provenance_for_test(candidate).unwrap())
            }
            CaptureRace::Namespace => Box::new(
                fixture
                    .fixture
                    .namespace_change_hook("activate-archived-capture-race".to_owned()),
            ),
        };
        arm_between_usr_rollback_activate_archived_complete_route_database_captures(hook);
        reset_candidate_observers();

        let error = enter_route(&fixture);

        assert_capture_race_pending(&error, race);
        assert_eq!(fixture.canonical_bytes(), canonical_before, "{race:?}");
        assert_eq!(fixture.canonical_record(), fixture.source, "{race:?}");
        assert_eq!(candidate_move_count(), 0, "{race:?}");
        match race {
            CaptureRace::Database => {
                assert_eq!(
                    fixture.fixture.fixture.database.all().unwrap().len(),
                    rows_before.len() - 1
                );
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            CaptureRace::Provenance => {
                assert_eq!(fixture.fixture.fixture.database.all().unwrap(), rows_before);
                assert!(
                    fixture
                        .fixture
                        .fixture
                        .database
                        .metadata_provenance(fixture.fixture.fixture.candidate_state)
                        .unwrap()
                        .is_none()
                );
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            CaptureRace::Namespace => {
                assert_eq!(fixture.fixture.fixture.database.all().unwrap(), rows_before);
                assert!(inserted.is_dir());
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FinalRace {
    Database,
    Provenance,
    Journal,
    Namespace,
}

#[test]
fn startup_activate_archived_complete_route_final_revalidation_rejects_database_provenance_journal_and_namespace_races()
{
    for race in [
        FinalRace::Database,
        FinalRace::Provenance,
        FinalRace::Journal,
        FinalRace::Namespace,
    ] {
        let fixture = exact_fixture();
        let expected = fixture.expected_successor();
        let rows_before = fixture.fixture.fixture.database.all().unwrap();
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        let inserted = fixture
            .fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("activate-archived-final-race");
        let hook: Box<dyn FnOnce()> = match race {
            FinalRace::Database => {
                let database = fixture.fixture.fixture.database.clone();
                let candidate = fixture.fixture.fixture.candidate_state;
                Box::new(move || database.remove(&candidate).unwrap())
            }
            FinalRace::Provenance => {
                let database = fixture.fixture.fixture.database.clone();
                let candidate = fixture.fixture.fixture.candidate_state;
                Box::new(move || database.delete_metadata_provenance_for_test(candidate).unwrap())
            }
            FinalRace::Journal => {
                let canonical = fixture
                    .fixture
                    .fixture
                    .installation
                    .root
                    .join(".cast/journal/state-transition");
                let changed = encode(&expected).unwrap();
                Box::new(move || fs::write(canonical, changed).unwrap())
            }
            FinalRace::Namespace => {
                let namespace_hook = fixture
                    .fixture
                    .namespace_change_hook("activate-archived-final-race".to_owned());
                Box::new(move || {
                    arm_before_usr_rollback_activate_archived_complete_route_fresh_namespace_capture(namespace_hook);
                })
            }
        };
        arm_before_usr_rollback_activate_archived_complete_route_final_revalidation(hook);
        reset_candidate_observers();

        let error = enter_route(&fixture);

        assert_complete_persistence_authority_error(&error);
        assert_eq!(candidate_move_count(), 0, "{race:?}");
        match race {
            FinalRace::Database => {
                assert_eq!(fixture.canonical_record(), fixture.source);
                assert_eq!(
                    fixture.fixture.fixture.database.all().unwrap().len(),
                    rows_before.len() - 1
                );
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            FinalRace::Provenance => {
                assert_eq!(fixture.canonical_record(), fixture.source);
                assert_eq!(fixture.fixture.fixture.database.all().unwrap(), rows_before);
                assert!(
                    fixture
                        .fixture
                        .fixture
                        .database
                        .metadata_provenance(fixture.fixture.fixture.candidate_state)
                        .unwrap()
                        .is_none()
                );
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            FinalRace::Journal => {
                assert_eq!(fixture.canonical_record(), expected);
                assert_eq!(fixture.database_snapshot(), database_before);
                assert_eq!(fixture.namespace_snapshot(), namespace_before);
            }
            FinalRace::Namespace => {
                assert_eq!(fixture.canonical_record(), fixture.source);
                assert_eq!(fixture.database_snapshot(), database_before);
                assert!(inserted.is_dir());
            }
        }
    }
}

fn assert_capture_race_pending(error: &startup_gate::Error, race: CaptureRace) {
    let startup_gate::Error::RecoveryPending(pending) = error else {
        panic!("expected {race:?} capture race to remain recovery-pending, got {error:?}");
    };
    assert_eq!(pending.phase(), Phase::CandidatePreserved);
    match race {
        CaptureRace::Database => assert!(pending.blockers().contains(&RecoveryBlocker::DatabaseConflict)),
        CaptureRace::Provenance => {
            assert!(
                pending
                    .blockers()
                    .contains(&RecoveryBlocker::MetadataProvenanceConflict)
            )
        }
        CaptureRace::Namespace => {}
    }
}

fn exact_fixture() -> RouteFixture {
    RouteFixture::new(
        Epoch::Historical,
        CandidateSource::Exchanged,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::Applied,
    )
}
