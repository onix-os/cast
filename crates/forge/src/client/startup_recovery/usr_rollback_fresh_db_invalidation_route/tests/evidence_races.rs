use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationRouteAdmission,
            arm_before_usr_rollback_fresh_db_invalidation_route_fresh_namespace_capture,
            arm_between_usr_rollback_fresh_db_invalidation_route_database_captures,
        },
        startup_recovery::{
            UsrRollbackFreshDbInvalidationRoutePersistenceError,
            arm_before_usr_rollback_fresh_db_invalidation_route_final_revalidation,
            persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
        },
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore, encode},
};

use super::support::{
    CandidateOutcome, CandidateSource, RouteFixture, canonical_journal, create_private_directory,
    transition_quarantine_path,
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
        Err(source) => panic!("inspect fresh-db route root ABI: {source}"),
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
        .prefix(".fresh-db-root-abi-displaced-")
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
fn startup_root_links_fresh_db_route_capture_rejects_all_root_abi_mutations() {
    for historical in [false, true] {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for candidate_outcome in CandidateOutcome::ALL {
                for (name, target) in ROOT_ABI {
                    for mutation in RootAbiMutation::ALL {
                        let fixture = RouteFixture::at_epoch(
                            historical,
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
                            "capture historical={historical} {usr_outcome:?} {candidate_outcome:?} {name} {mutation:?}"
                        );
                        let hook = root_abi_mutation_hook(
                            &fixture,
                            name,
                            target,
                            mutation,
                            case.clone(),
                        );
                        arm_between_usr_rollback_fresh_db_invalidation_route_database_captures(hook);

                        assert!(matches!(
                            fixture.capture(&journal, &reservation).unwrap(),
                            UsrRollbackFreshDbInvalidationRouteAdmission::Deferred
                        ));

                        assert_eq!(fixture.canonical_bytes(), canonical_before);
                        assert_eq!(fixture.database_snapshot(), database_before);
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

#[test]
fn startup_root_links_fresh_db_route_final_revalidation_rejects_all_root_abi_mutations() {
    for historical in [false, true] {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for candidate_outcome in CandidateOutcome::ALL {
                for (name, target) in ROOT_ABI {
                    for mutation in RootAbiMutation::ALL {
                        let fixture = RouteFixture::at_epoch(
                            historical,
                            CandidateSource::RootLinksComplete,
                            usr_outcome,
                            candidate_outcome,
                        );
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = fixture.capture_ready(&journal, &reservation);
                        let canonical_before = fixture.canonical_bytes();
                        let database_before = fixture.database_snapshot();
                        let root = fixture.fixture.fixture.installation.root.clone();
                        let root_abi_before = root_abi_snapshot(&root);
                        let case = format!(
                            "final historical={historical} {usr_outcome:?} {candidate_outcome:?} {name} {mutation:?}"
                        );
                        let hook = root_abi_mutation_hook(
                            &fixture,
                            name,
                            target,
                            mutation,
                            case.clone(),
                        );
                        arm_before_usr_rollback_fresh_db_invalidation_route_final_revalidation(move || {
                            arm_before_usr_rollback_fresh_db_invalidation_route_fresh_namespace_capture(hook);
                        });

                        let error =
                            persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)
                                .unwrap_err();

                        assert!(matches!(
                            error,
                            UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(_)
                        ));
                        assert_eq!(fixture.canonical_bytes(), canonical_before);
                        assert_eq!(fixture.database_snapshot(), database_before);
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

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_rejects_mixed_and_cross_root_journals() {
    for candidate_outcome in CandidateOutcome::ALL {
        let fixture = RouteFixture::new(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            candidate_outcome,
        );
        let first = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&first, &reservation);
        drop(first);
        let independently_reopened = fixture.open_journal();

        let error =
            persist_usr_rollback_fresh_db_invalidation_route_and_reopen(independently_reopened, authority).unwrap_err();

        assert!(matches!(
            error,
            UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(_)
        ));
        assert_eq!(fixture.canonical_record(), fixture.source);
        drop(reservation);

        let first = RouteFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            candidate_outcome,
        );
        let second = RouteFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            candidate_outcome,
        );
        fs::write(
            canonical_journal(&second.fixture.fixture.installation.root),
            first.canonical_bytes(),
        )
        .unwrap();
        let first_journal = first.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = first.capture_ready(&first_journal, &reservation);
        let foreign = TransitionJournalStore::open_retained(
            second.fixture.fixture.installation.root_directory(),
            &second.fixture.fixture.installation.root,
        )
        .unwrap();

        let error = persist_usr_rollback_fresh_db_invalidation_route_and_reopen(foreign, authority).unwrap_err();

        assert!(matches!(
            error,
            UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(_)
        ));
        assert_eq!(first_journal.load().unwrap(), Some(first.source.clone()));
        assert_eq!(first.canonical_record(), first.source);
        assert_eq!(second.canonical_record(), first.source);
    }
}

#[derive(Clone, Copy, Debug)]
enum FinalRace {
    Database,
    Provenance,
    Journal,
    Installation,
    Namespace,
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_capture_and_final_evidence_races_never_advance() {
    let fixture = RouteFixture::new(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateOutcome::Applied,
    );
    let database = fixture.fixture.fixture.database.clone();
    let candidate = fixture.fixture.fixture.candidate_state;
    let transition = fixture.source.transition_id.clone();
    arm_between_usr_rollback_fresh_db_invalidation_route_database_captures(move || {
        database.clear_transition_if_matches(candidate, &transition).unwrap();
    });
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        fixture.capture(&journal, &reservation).unwrap(),
        UsrRollbackFreshDbInvalidationRouteAdmission::Deferred
    ));
    assert_eq!(fixture.canonical_record(), fixture.source);
    drop(journal);
    drop(reservation);

    for race in [
        FinalRace::Database,
        FinalRace::Provenance,
        FinalRace::Journal,
        FinalRace::Installation,
        FinalRace::Namespace,
    ] {
        let fixture = RouteFixture::new(
            CandidateSource::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateOutcome::AlreadySatisfied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        arm_final_race(&fixture, race);

        let error = persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap_err();

        assert!(
            matches!(error, UsrRollbackFreshDbInvalidationRoutePersistenceError::Authority(_)),
            "{race:?}: {error:?}"
        );
    }
}

fn arm_final_race(fixture: &RouteFixture, race: FinalRace) {
    let hook: Box<dyn FnOnce()> = match race {
        FinalRace::Database => {
            let database = fixture.fixture.fixture.database.clone();
            let candidate = fixture.fixture.fixture.candidate_state;
            let transition = fixture.source.transition_id.clone();
            Box::new(move || {
                database.clear_transition_if_matches(candidate, &transition).unwrap();
            })
        }
        FinalRace::Provenance => {
            let database = fixture.fixture.fixture.database.clone();
            let candidate = fixture.fixture.fixture.candidate_state;
            Box::new(move || {
                database.delete_metadata_provenance_for_test(candidate).unwrap();
            })
        }
        FinalRace::Journal => {
            let canonical = canonical_journal(&fixture.fixture.fixture.installation.root);
            let changed = encode(&fixture.expected_successor()).unwrap();
            Box::new(move || fs::write(canonical, changed).unwrap())
        }
        FinalRace::Installation => {
            let cast = fixture.fixture.fixture.installation.root.join(".cast");
            let displaced = fixture
                .fixture
                .fixture
                .installation
                .root
                .join(".cast-fresh-db-route-rebound");
            Box::new(move || {
                fs::rename(&cast, displaced).unwrap();
                fs::create_dir(&cast).unwrap();
                fs::set_permissions(cast, fs::Permissions::from_mode(0o700)).unwrap();
            })
        }
        FinalRace::Namespace => {
            let target = transition_quarantine_path(&fixture.fixture.fixture, &fixture.source);
            Box::new(move || {
                arm_before_usr_rollback_fresh_db_invalidation_route_fresh_namespace_capture(move || {
                    fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap();
                });
            })
        }
    };
    arm_before_usr_rollback_fresh_db_invalidation_route_final_revalidation(hook);
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_refuses_namespace_lookalikes() {
    for case in 0..3 {
        let fixture = RouteFixture::new(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOutcome::AlreadySatisfied,
        );
        let target = transition_quarantine_path(&fixture.fixture.fixture, &fixture.source);
        match case {
            0 => fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap(),
            1 => fs::rename(
                &target,
                fixture
                    .fixture
                    .fixture
                    .installation
                    .state_quarantine_dir()
                    .join("displaced-fresh-db-route-target"),
            )
            .unwrap(),
            2 => create_private_directory(
                &fixture
                    .fixture
                    .fixture
                    .installation
                    .root
                    .join(".cast/root")
                    .join(fixture.fixture.fixture.candidate_state.to_string()),
            ),
            _ => unreachable!(),
        }
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        assert!(
            matches!(
                fixture.capture(&journal, &reservation).unwrap(),
                UsrRollbackFreshDbInvalidationRouteAdmission::Deferred
            ),
            "namespace lookalike {case} was admitted"
        );
        assert_eq!(fixture.canonical_record(), fixture.source);
    }
}
