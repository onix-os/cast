use std::{
    fs,
    os::unix::fs::{MetadataExt as _, symlink},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate,
        startup_reconciliation::{
            arm_before_usr_rollback_resume_route_fresh_namespace_capture,
            arm_between_usr_rollback_resume_route_database_captures,
        },
        startup_recovery::{
            UsrRollbackResumeRoutePersistenceError, arm_before_usr_rollback_resume_route_final_revalidation,
            persist_usr_rollback_resume_route_and_reopen,
        },
    },
    transition_journal::{ForwardPhase, Phase, RollbackActionOutcome},
};

use super::{
    fixture::{OperationKind, ROOT_ABI, SourceCase, create_private_directory, pending},
    support::RouteFixture,
};

#[derive(Clone, Copy, Debug)]
enum RootAbiRouteSeam {
    FreshNamespaceCapture,
    FinalRevalidation,
}

impl RootAbiRouteSeam {
    const ALL: [Self; 2] = [Self::FreshNamespaceCapture, Self::FinalRevalidation];
}

#[derive(Clone, Copy, Debug)]
enum RootAbiRace {
    Missing,
    WrongTarget,
    SameTargetDifferentInode,
}

impl RootAbiRace {
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

#[test]
fn startup_usr_rollback_resume_route_rejects_a_different_open_journal_binding() {
    let fixture = RouteFixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let first_journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&first_journal, &reservation);
    let before = fixture.fixture.canonical_bytes();
    drop(first_journal);

    let independently_reopened = fixture.open_journal();
    let error = persist_usr_rollback_resume_route_and_reopen(independently_reopened, authority).unwrap_err();

    assert!(matches!(error, UsrRollbackResumeRoutePersistenceError::Authority(_)));
    assert_eq!(fixture.fixture.canonical_bytes(), before);
    assert_eq!(fixture.canonical_record(), fixture.source);
}

#[test]
fn startup_usr_rollback_resume_route_database_and_provenance_conflicts_never_advance() {
    for kind in OperationKind::ALL {
        let fixture = RouteFixture::new(kind, SourceCase::ExchangedPost);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let before = fixture.fixture.canonical_bytes();
        if kind == OperationKind::NewState {
            fixture
                .fixture
                .database
                .clear_transition_if_matches(fixture.fixture.candidate_state, &fixture.source.transition_id)
                .unwrap();
        } else {
            fixture
                .fixture
                .database
                .remove(&fixture.fixture.candidate_state)
                .unwrap();
        }
        let error = persist_usr_rollback_resume_route_and_reopen(journal, authority).unwrap_err();
        assert!(
            matches!(error, UsrRollbackResumeRoutePersistenceError::Authority(_)),
            "{kind:?}: {error:?}"
        );
        assert_eq!(fixture.fixture.canonical_bytes(), before, "{kind:?}");
        assert_eq!(fixture.canonical_record(), fixture.source, "{kind:?}");
    }

    let fixture = RouteFixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let before = fixture.fixture.canonical_bytes();
    fixture
        .fixture
        .database
        .delete_metadata_provenance_for_test(fixture.fixture.candidate_state)
        .unwrap();
    let error = persist_usr_rollback_resume_route_and_reopen(journal, authority).unwrap_err();
    assert!(matches!(error, UsrRollbackResumeRoutePersistenceError::Authority(_)));
    assert_eq!(fixture.fixture.canonical_bytes(), before);
    drop(reservation);

    let fixture = RouteFixture::usr_restored(
        OperationKind::NewState,
        SourceCase::ExchangedPost,
        RollbackActionOutcome::Applied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let before = fixture.fixture.canonical_bytes();
    fixture
        .fixture
        .database
        .clear_transition_if_matches(fixture.fixture.candidate_state, &fixture.source.transition_id)
        .unwrap();
    let error = persist_usr_rollback_resume_route_and_reopen(journal, authority).unwrap_err();
    assert!(matches!(error, UsrRollbackResumeRoutePersistenceError::Authority(_)));
    assert_eq!(fixture.fixture.canonical_bytes(), before);
    drop(reservation);

    let fixture = RouteFixture::usr_restored(
        OperationKind::Archived,
        SourceCase::IntentPost,
        RollbackActionOutcome::AlreadySatisfied,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let before = fixture.fixture.canonical_bytes();
    fixture
        .fixture
        .database
        .delete_metadata_provenance_for_test(fixture.fixture.candidate_state)
        .unwrap();
    let error = persist_usr_rollback_resume_route_and_reopen(journal, authority).unwrap_err();
    assert!(matches!(error, UsrRollbackResumeRoutePersistenceError::Authority(_)));
    assert_eq!(fixture.fixture.canonical_bytes(), before);
}

#[test]
fn startup_usr_rollback_resume_route_namespace_conflicts_never_advance() {
    let fixture = RouteFixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let before = fixture.fixture.canonical_bytes();
    create_private_directory(
        &fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join("rollback-route-ambient"),
    );
    let error = persist_usr_rollback_resume_route_and_reopen(journal, authority).unwrap_err();
    assert!(matches!(error, UsrRollbackResumeRoutePersistenceError::Authority(_)));
    assert_eq!(fixture.fixture.canonical_bytes(), before);
    drop(reservation);

    let fixture = RouteFixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = fixture.capture_ready(&journal, &reservation);
    let before = fixture.fixture.canonical_bytes();
    fs::remove_file(fixture.fixture.installation.isolation_path("bin")).unwrap();
    let error = persist_usr_rollback_resume_route_and_reopen(journal, authority).unwrap_err();
    assert!(matches!(error, UsrRollbackResumeRoutePersistenceError::Authority(_)));
    assert_eq!(fixture.fixture.canonical_bytes(), before);
    drop(reservation);

    let fixture = RouteFixture::usr_restored(
        OperationKind::Archived,
        SourceCase::ExchangedPost,
        RollbackActionOutcome::Applied,
    );
    let before = fixture.fixture.canonical_bytes();
    create_private_directory(
        &fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join(fixture.source.quarantine_name.as_str()),
    );
    let deferred = fixture.enter();
    assert_eq!(pending(&deferred).phase(), Phase::UsrRestored);
    assert_eq!(fixture.fixture.canonical_bytes(), before);
}

#[test]
fn startup_usr_rollback_resume_route_capture_and_final_revalidation_races_fail_before_advance() {
    let fixture = RouteFixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    let before = fixture.fixture.canonical_bytes();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    let transition = fixture.source.transition_id.clone();
    arm_between_usr_rollback_resume_route_database_captures(move || {
        database.clear_transition_if_matches(candidate, &transition).unwrap();
    });
    let error = fixture.enter();
    assert_eq!(pending(&error).phase(), Phase::RollbackDecided);
    assert_eq!(fixture.fixture.canonical_bytes(), before);

    let fixture = RouteFixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let before = fixture.fixture.canonical_bytes();
    let inserted = fixture
        .fixture
        .installation
        .state_quarantine_dir()
        .join("rollback-route-capture-race");
    arm_before_usr_rollback_resume_route_fresh_namespace_capture(move || {
        create_private_directory(&inserted);
    });
    assert_authority_failure(fixture.enter());
    assert_eq!(fixture.fixture.canonical_bytes(), before);

    let fixture = RouteFixture::new(OperationKind::ActiveReblit, SourceCase::ExchangedPost);
    let before = fixture.fixture.canonical_bytes();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    arm_before_usr_rollback_resume_route_final_revalidation(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });
    assert_authority_failure(fixture.enter());
    assert_eq!(fixture.fixture.canonical_bytes(), before);

    let fixture = RouteFixture::usr_restored(
        OperationKind::ActiveReblit,
        SourceCase::IntentPost,
        RollbackActionOutcome::AlreadySatisfied,
    );
    let before = fixture.fixture.canonical_bytes();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    arm_before_usr_rollback_resume_route_final_revalidation(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });
    assert_authority_failure(fixture.enter());
    assert_eq!(fixture.fixture.canonical_bytes(), before);
}

#[test]
fn startup_root_links_complete_usr_restored_root_abi_races_reject_both_route_seams_without_advance() {
    let mut fresh_namespace_capture_cases = 0;
    let mut final_revalidation_cases = 0;

    for seam in RootAbiRouteSeam::ALL {
        for historical in [false, true] {
            for kind in OperationKind::ALL {
                for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for link_index in 0..ROOT_ABI.len() {
                        for race in RootAbiRace::ALL {
                            let fixture = RouteFixture::usr_restored_at_epoch(
                                kind,
                                SourceCase::RootLinksCompletePost,
                                outcome,
                                historical,
                            );
                            let case = format!(
                                "{seam:?} historical={historical} {kind:?} {outcome:?} {} {race:?}",
                                ROOT_ABI[link_index].0
                            );
                            assert_eq!(fixture.source.phase, Phase::UsrRestored, "{case}");
                            assert_eq!(
                                fixture.source.rollback.as_ref().unwrap().source,
                                ForwardPhase::RootLinksComplete,
                                "{case}"
                            );
                            let journal_before = fixture.fixture.canonical_bytes();
                            let database_before = fixture.fixture.database_snapshot();
                            let root_abi_before = root_abi_snapshot(&fixture.fixture.installation.root);
                            let root = fixture.fixture.installation.root.clone();
                            let hook = move || apply_root_abi_race(&root, link_index, race);
                            match seam {
                                RootAbiRouteSeam::FreshNamespaceCapture => {
                                    arm_before_usr_rollback_resume_route_fresh_namespace_capture(hook);
                                    fresh_namespace_capture_cases += 1;
                                }
                                RootAbiRouteSeam::FinalRevalidation => {
                                    arm_before_usr_rollback_resume_route_final_revalidation(hook);
                                    final_revalidation_cases += 1;
                                }
                            }

                            assert_authority_failure(fixture.enter());

                            assert_eq!(fixture.fixture.canonical_bytes(), journal_before, "{case}");
                            assert_eq!(fixture.canonical_record(), fixture.source, "{case}");
                            assert_eq!(fixture.fixture.database_snapshot(), database_before, "{case}");
                            assert_exact_root_abi_race(
                                &root_abi_before,
                                &root_abi_snapshot(&fixture.fixture.installation.root),
                                link_index,
                                race,
                                &case,
                            );
                        }
                    }
                }
            }
        }
    }

    assert_eq!(fresh_namespace_capture_cases, 180);
    assert_eq!(final_revalidation_cases, 180);
    assert_eq!(fresh_namespace_capture_cases + final_revalidation_cases, 360);
}

#[test]
fn startup_usr_rollback_resume_route_historical_and_active_reblit_evidence_remain_exact() {
    for kind in OperationKind::ALL {
        let fixture = RouteFixture::historical(kind, SourceCase::ExchangedPost);
        let epoch = fixture.source.creation_epoch.clone();
        let error = fixture.enter();
        assert_eq!(pending(&error).phase(), Phase::ReverseExchangeIntent, "{kind:?}");
        let actual = fixture.canonical_record();
        fixture.assert_exact_route(&actual);
        assert_eq!(actual.creation_epoch, epoch, "{kind:?}");
    }

    for source in [
        SourceCase::IntentPre,
        SourceCase::ExchangedPost,
        SourceCase::RootLinksCompletePost,
    ] {
        let fixture = RouteFixture::new(OperationKind::ActiveReblit, source);
        assert_eq!(fixture.fixture.candidate_state, fixture.fixture.previous_state);
        assert_eq!(fixture.fixture.database.all().unwrap().len(), 1);
        let database_before = fixture.fixture.database_snapshot();
        let reservation_path = fixture
            .fixture
            .active_reblit_reservation
            .as_ref()
            .expect("active-reblit fixture retains its replacement reservation");
        let reservation_before = fs::symlink_metadata(reservation_path).unwrap();

        let contender_acquired = Arc::new(AtomicBool::new(false));
        let contender_acquired_in_thread = Arc::clone(&contender_acquired);
        let contender = Arc::new(Mutex::new(None));
        let contender_in_hook = Arc::clone(&contender);
        arm_before_usr_rollback_resume_route_final_revalidation(move || {
            let (started_tx, started_rx) = mpsc::channel();
            let acquired_by_contender = Arc::clone(&contender_acquired_in_thread);
            let handle = thread::spawn(move || {
                started_tx.send(()).unwrap();
                let reservation = ActiveStateReservation::acquire().unwrap();
                acquired_by_contender.store(true, Ordering::SeqCst);
                drop(reservation);
            });
            *contender_in_hook.lock().unwrap() = Some(handle);
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            thread::sleep(Duration::from_millis(50));
            assert!(
                !contender_acquired_in_thread.load(Ordering::SeqCst),
                "cooperating writer acquired during rollback-resume routing"
            );
        });

        let error = fixture.enter();
        assert_eq!(pending(&error).phase(), fixture.expected_phase(), "{source:?}");
        fixture.assert_exact_route(&fixture.canonical_record());

        for _ in 0..100 {
            if contender_acquired.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            contender_acquired.load(Ordering::SeqCst),
            "cooperating writer did not acquire after startup returned"
        );
        contender
            .lock()
            .unwrap()
            .take()
            .expect("final-revalidation hook spawned a contender")
            .join()
            .unwrap();

        let reservation_after = fs::symlink_metadata(reservation_path).unwrap();
        assert_eq!(
            (
                reservation_after.dev(),
                reservation_after.ino(),
                reservation_after.mode()
            ),
            (
                reservation_before.dev(),
                reservation_before.ino(),
                reservation_before.mode()
            )
        );
        assert_eq!(fixture.fixture.database.all().unwrap().len(), 1);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
    }
}

fn root_abi_snapshot(root: &Path) -> [Option<RootAbiLinkIdentity>; 5] {
    ROOT_ABI.map(|(name, _)| {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => Some(RootAbiLinkIdentity {
                target: fs::read_link(path).unwrap(),
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                links: metadata.nlink(),
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => panic!("inspect root ABI route-race fixture: {source}"),
        }
    })
}

fn apply_root_abi_race(root: &Path, link_index: usize, race: RootAbiRace) {
    let (name, target) = ROOT_ABI[link_index];
    let link = root.join(name);
    fs::remove_file(&link).unwrap();
    match race {
        RootAbiRace::Missing => {}
        RootAbiRace::WrongTarget => symlink(format!("usr/not-{name}"), link).unwrap(),
        RootAbiRace::SameTargetDifferentInode => symlink(target, link).unwrap(),
    }
}

fn assert_exact_root_abi_race(
    before: &[Option<RootAbiLinkIdentity>; 5],
    after: &[Option<RootAbiLinkIdentity>; 5],
    link_index: usize,
    race: RootAbiRace,
    case: &str,
) {
    for (index, (_, expected_target)) in ROOT_ABI.into_iter().enumerate() {
        let original = before[index]
            .as_ref()
            .unwrap_or_else(|| panic!("root ABI fixture link {index} was missing before {case}"));
        assert_eq!(original.target, PathBuf::from(expected_target), "{case}");
        if index != link_index {
            assert_eq!(after[index].as_ref(), Some(original), "{case}");
        }
    }

    let original = before[link_index].as_ref().unwrap();
    match race {
        RootAbiRace::Missing => assert!(after[link_index].is_none(), "{case}"),
        RootAbiRace::WrongTarget => {
            let changed = after[link_index].as_ref().unwrap();
            assert_eq!(
                changed.target,
                PathBuf::from(format!("usr/not-{}", ROOT_ABI[link_index].0)),
                "{case}"
            );
            assert_eq!(changed.device, original.device, "{case}");
            assert_ne!(changed.inode, original.inode, "{case}");
            assert_eq!(changed.mode, original.mode, "{case}");
            assert_eq!(changed.links, original.links, "{case}");
        }
        RootAbiRace::SameTargetDifferentInode => {
            let changed = after[link_index].as_ref().unwrap();
            assert_eq!(changed.target, original.target, "{case}");
            assert_eq!(changed.device, original.device, "{case}");
            assert_ne!(changed.inode, original.inode, "{case}");
            assert_eq!(changed.mode, original.mode, "{case}");
            assert_eq!(changed.links, original.links, "{case}");
        }
    }
}

fn assert_authority_failure(error: startup_gate::Error) {
    assert!(matches!(
        error,
        startup_gate::Error::UsrRollbackResumeRoutePersistence(UsrRollbackResumeRoutePersistenceError::Authority(_))
    ));
}
