const COORDINATOR_OS_INFO: &[u8] = b"name = \"Exact Coordinator OS\"\nversion = \"1\"\n";

#[derive(Debug, Eq, PartialEq)]
struct CandidateMetadataEntryEvidence {
    relative: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    bytes: Vec<u8>,
}

fn candidate_metadata_evidence(fixture: &CoordinatorFixture) -> Vec<CandidateMetadataEntryEvidence> {
    ["lib", "lib/os-release", "lib/system-model.glu"]
        .into_iter()
        .map(|relative| {
            let path = fixture.candidate_path.join(relative);
            let metadata = fs::symlink_metadata(&path).unwrap();
            CandidateMetadataEntryEvidence {
                relative: PathBuf::from(relative),
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.permissions().mode() & 0o7777,
                links: metadata.nlink(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
                bytes: metadata
                    .file_type()
                    .is_file()
                    .then(|| fs::read(path).unwrap())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn park_and_replace_metadata(fixture: &CoordinatorFixture, name: &str) -> PathBuf {
    let canonical = fixture.candidate_path.join("lib").join(name);
    let bytes = fs::read(&canonical).unwrap();
    let parked = fixture.candidate_path.parent().unwrap().join(format!("parked-{name}"));
    fs::rename(&canonical, &parked).unwrap();
    write_canonical_file(&canonical, &bytes);
    assert_ne!(
        fs::symlink_metadata(&canonical).unwrap().ino(),
        fs::symlink_metadata(&parked).unwrap().ino()
    );
    parked
}

fn copy_flat_candidate(source: &Path, destination: &Path) {
    create_canonical_directory(destination);
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let metadata = entry.metadata().unwrap();
        assert!(metadata.file_type().is_file(), "fixture candidate must remain flat");
        let destination_entry = destination.join(entry.file_name());
        fs::copy(entry.path(), &destination_entry).unwrap();
        fs::set_permissions(
            &destination_entry,
            fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777),
        )
        .unwrap();
    }
}

#[test]
fn journal_coordinator_metadata_proof_is_owned_for_every_operation_and_uses_exact_os_info() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(candidate_kind);
        if candidate_kind != CandidateKind::Archived {
            create_canonical_directory(&fixture.candidate_path.join("lib"));
        }
        write_canonical_file(
            &fixture.candidate_path.join("lib/os-info.json"),
            COORDINATOR_OS_INFO,
        );
        let existing_before =
            (candidate_kind == CandidateKind::Archived).then(|| candidate_metadata_evidence(&fixture));
        let prepared = coordinator
            .finish_candidate_prepare(COORDINATOR_SYSTEM_SNAPSHOT, |actual| {
                assert_eq!(actual, Some(COORDINATOR_OS_INFO));
                COORDINATOR_OS_RELEASE.to_vec()
            })
            .unwrap();

        assert_eq!(prepared.record().phase, Phase::CandidatePrepared);
        assert_candidate_metadata(&fixture);
        if let Some(existing_before) = existing_before {
            assert_eq!(candidate_metadata_evidence(&fixture), existing_before);
        }
        match (candidate_kind, prepared) {
            (
                CandidateKind::NewState | CandidateKind::ActiveReblit,
                PreparedStatefulTransitionCoordinator::TransactionTriggers(_),
            ) => {}
            (CandidateKind::Archived, PreparedStatefulTransitionCoordinator::Archived(_)) => {}
            _ => panic!("candidate operation received the wrong proof-bearing authority"),
        }
    }
}

#[test]
fn journal_coordinator_archived_metadata_proof_rejects_independent_expectation_mismatch_without_mutation() {
    let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::Archived);
    let started = coordinator.record().clone();
    let before = candidate_metadata_evidence(&fixture);

    let failure = coordinator
        .finish_candidate_prepare(b"independently expected but incorrect snapshot\n", |_| {
            COORDINATOR_OS_RELEASE.to_vec()
        })
        .unwrap_err();
    assert!(matches!(
        failure,
        StatefulTransitionCoordinatorError::CandidateMetadata(_)
    ));
    assert_eq!(read_canonical(&fixture.installation.root), started);
    assert_eq!(candidate_metadata_evidence(&fixture), before);
    assert_candidate_state_id(&fixture, fixture.candidate_state);
}

#[test]
fn journal_coordinator_candidate_prepare_rejects_same_byte_foreign_candidate_before_metadata_or_state_id() {
    let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
    let started = coordinator.record().clone();
    let retained = fixture.candidate_path.with_file_name("retained-candidate-usr");
    fs::rename(&fixture.candidate_path, &retained).unwrap();
    copy_flat_candidate(&retained, &fixture.candidate_path);

    let failure = finish_candidate_prepare(coordinator).unwrap_err();
    assert!(matches!(failure, StatefulTransitionCoordinatorError::Identity(_)));
    assert_eq!(reopen_record(&fixture.installation.root), started);
    assert!(!fixture.candidate_path.join("lib").exists());
    assert!(!retained.join("lib").exists());
    assert_state_metadata_name_absent(&fixture.candidate_path.join(".stateID"));
    assert_state_metadata_name_absent(&retained.join(".stateID"));
    assert_new_state_payload_sentinel(&fixture);
    assert_eq!(
        fs::read(retained.join("payload-sentinel")).unwrap(),
        NEW_STATE_PAYLOAD_SENTINEL
    );
}

#[test]
fn journal_coordinator_metadata_substitution_before_trigger_intent_runs_no_effect() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let prepared = coordinator.record().clone();
    let parked = park_and_replace_metadata(&fixture, "os-release");
    let calls = std::cell::Cell::new(0usize);

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();
    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::Preflight {
            source: StatefulTransitionCoordinatorError::CandidateMetadata(_),
            ..
        }
    ));
    assert_eq!(calls.get(), 0);
    assert_eq!(read_canonical(&fixture.installation.root), prepared);
    assert_eq!(fs::read(parked).unwrap(), COORDINATOR_OS_RELEASE);
    assert_eq!(
        fs::read(fixture.candidate_path.join("lib/os-release")).unwrap(),
        COORDINATOR_OS_RELEASE
    );
}

#[test]
fn journal_coordinator_metadata_substitution_during_trigger_effect_stops_before_completion() {
    let (fixture, coordinator) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let transition = coordinator.record().transition_id.clone();
    let calls = std::cell::Cell::new(0usize);
    let mut parked = None;

    let failure = coordinator
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            parked = Some(park_and_replace_metadata(&fixture, "system-model.glu"));
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();
    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::PostEffectEvidence {
            transition_id,
            source: StatefulTransitionCoordinatorError::CandidateMetadata(_),
        } if transition_id == transition
    ));
    assert_eq!(calls.get(), 1);
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::NewState,
        Phase::TransactionTriggersStarted,
        6,
    );
    assert_eq!(fs::read(parked.unwrap()).unwrap(), COORDINATOR_SYSTEM_SNAPSHOT);
}

#[test]
fn journal_coordinator_metadata_publication_failure_releases_authorities_while_error_lives() {
    let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
    let started = coordinator.record().clone();
    let release = fixture.candidate_path.join("lib/os-release");
    let parked = fixture.candidate_path.parent().unwrap().join("failed-publication-os-release");
    crate::transition_identity::arm_after_candidate_metadata_first_publication(move || {
        fs::rename(&release, &parked).unwrap();
        write_canonical_file(&release, COORDINATOR_OS_RELEASE);
    });

    let failure = finish_candidate_prepare(coordinator).unwrap_err();
    assert!(matches!(
        failure,
        StatefulTransitionCoordinatorError::CandidateMetadata(_)
    ));
    assert_candidate_state_id_absent(&fixture);

    let root = fixture.installation.root.clone();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let record = TransitionJournalStore::open(&root).unwrap().load().unwrap().unwrap();
        sender.send(record).unwrap();
    });
    assert_eq!(
        receiver.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(started),
        "a returned metadata failure retained coordinator authority"
    );
    worker.join().unwrap();
    drop(failure);
}
