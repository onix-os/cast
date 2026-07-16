enum TestUsrExchangeReady {
    TransactionTriggers(TransactionTriggersCompleteCoordinator),
    Archived(PreparedArchivedTransitionCoordinator),
}

impl TestUsrExchangeReady {
    fn begin(self) -> Result<UsrExchangeIntentCoordinator, UsrExchangeIntentFailure> {
        match self {
            Self::TransactionTriggers(coordinator) => coordinator.begin_usr_exchange_intent(),
            Self::Archived(coordinator) => coordinator.begin_usr_exchange_intent(),
        }
    }

    fn record(&self) -> &TransitionRecord {
        match self {
            Self::TransactionTriggers(coordinator) => coordinator.record(),
            Self::Archived(coordinator) => coordinator.record(),
        }
    }
}

fn coordinator_ready_for_usr_exchange(
    candidate_kind: CandidateKind,
) -> (CoordinatorFixture, TestUsrExchangeReady) {
    if candidate_kind == CandidateKind::Archived {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(candidate_kind);
        let prepared = finish_candidate_prepare(coordinator).unwrap();
        let PreparedStatefulTransitionCoordinator::Archived(prepared) = prepared else {
            panic!("archived activation acquired transaction-trigger authority")
        };
        return (fixture, TestUsrExchangeReady::Archived(prepared));
    }

    let (fixture, prepared) = coordinator_at_candidate_prepared(candidate_kind);
    let complete = prepared
        .run_transaction_triggers(|_| Ok::<(), TriggerEffectError>(()))
        .unwrap();
    (
        fixture,
        TestUsrExchangeReady::TransactionTriggers(complete),
    )
}

fn expected_usr_exchange_predecessor(candidate_kind: CandidateKind) -> (Operation, Phase, u64, u64) {
    match candidate_kind {
        CandidateKind::NewState => (
            Operation::NewState,
            Phase::TransactionTriggersComplete,
            7,
            8,
        ),
        CandidateKind::Archived => (
            Operation::ActivateArchived,
            Phase::CandidatePrepared,
            3,
            4,
        ),
        CandidateKind::ActiveReblit => (
            Operation::ActiveReblit,
            Phase::TransactionTriggersComplete,
            5,
            6,
        ),
    }
}

fn assert_usr_exchange_preflight_failure(
    failure: UsrExchangeIntentFailure,
    transition_id: &TransitionId,
    predecessor: Phase,
) {
    assert!(matches!(
        failure,
        UsrExchangeIntentFailure::Preflight {
            transition_id: actual_transition,
            predecessor: actual_predecessor,
            ..
        } if actual_transition == *transition_id && actual_predecessor == predecessor
    ));
}

fn replace_file_with_same_bytes(path: &Path, displaced_name: &str) {
    let bytes = fs::read(path).unwrap();
    let displaced = path.with_file_name(displaced_name);
    fs::rename(path, &displaced).unwrap();
    write_canonical_file(path, &bytes);
    let replacement = fs::symlink_metadata(path).unwrap();
    let original = fs::symlink_metadata(displaced).unwrap();
    assert_ne!((replacement.dev(), replacement.ino()), (original.dev(), original.ino()));
}

fn directory_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    assert!(metadata.is_dir());
    (metadata.dev(), metadata.ino())
}

#[test]
fn journal_coordinator_usr_exchange_intent_has_exact_phase_and_generation_for_every_operation() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        let (fixture, ready) = coordinator_ready_for_usr_exchange(candidate_kind);
        let (operation, predecessor, predecessor_generation, intent_generation) =
            expected_usr_exchange_predecessor(candidate_kind);
        assert_record_prefix(ready.record(), operation, predecessor, predecessor_generation);

        let intent = ready.begin().unwrap();
        assert_record_prefix(intent.record(), operation, Phase::UsrExchangeIntent, intent_generation);
        assert_eq!(read_canonical(&fixture.installation.root), *intent.record());
    }
}

#[test]
fn journal_coordinator_usr_exchange_intent_performs_no_exchange_or_root_link_publication() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        let (fixture, ready) = coordinator_ready_for_usr_exchange(candidate_kind);
        let live_usr = fixture.installation.root.join("usr");
        let candidate_before = directory_identity(&fixture.candidate_path);
        let previous_before = directory_identity(&live_usr);
        let root_abi_names = ["bin", "sbin", "lib", "lib32", "lib64"];
        for name in root_abi_names {
            assert_state_metadata_name_absent(&fixture.installation.root.join(name));
        }

        let intent = ready.begin().unwrap();

        assert_eq!(directory_identity(&fixture.candidate_path), candidate_before);
        assert_eq!(directory_identity(&live_usr), previous_before);
        assert_ne!(candidate_before, previous_before);
        for name in root_abi_names {
            assert_state_metadata_name_absent(&fixture.installation.root.join(name));
        }
        assert_eq!(intent.record().phase, Phase::UsrExchangeIntent);
    }
}

#[test]
fn journal_coordinator_usr_exchange_intent_revalidates_all_retained_evidence_before_advance() {
    {
        let (fixture, ready) = coordinator_ready_for_usr_exchange(CandidateKind::NewState);
        let predecessor = ready.record().clone();
        replace_file_with_same_bytes(
            &fixture.candidate_path.join("lib/os-release"),
            "os-release.intent-displaced",
        );
        let failure = ready.begin().unwrap_err();
        assert_usr_exchange_preflight_failure(failure, &predecessor.transition_id, predecessor.phase);
        assert_eq!(read_canonical(&fixture.installation.root), predecessor);
    }

    {
        let (fixture, ready) = coordinator_ready_for_usr_exchange(CandidateKind::Archived);
        let predecessor = ready.record().clone();
        let live_usr = fixture.installation.root.join("usr");
        let displaced = fixture.installation.root.join("usr.intent-displaced");
        fs::rename(&live_usr, &displaced).unwrap();
        create_canonical_directory(&live_usr);
        let failure = ready.begin().unwrap_err();
        assert_usr_exchange_preflight_failure(failure, &predecessor.transition_id, predecessor.phase);
        assert_eq!(read_canonical(&fixture.installation.root), predecessor);
    }

    {
        let (fixture, ready) = coordinator_ready_for_usr_exchange(CandidateKind::ActiveReblit);
        let predecessor = ready.record().clone();
        replace_file_with_same_bytes(&state_id_path(&fixture), ".stateID.intent-displaced");
        let failure = ready.begin().unwrap_err();
        assert_usr_exchange_preflight_failure(failure, &predecessor.transition_id, predecessor.phase);
        assert_eq!(read_canonical(&fixture.installation.root), predecessor);
    }

    {
        let (fixture, ready) = coordinator_ready_for_usr_exchange(CandidateKind::NewState);
        let predecessor = ready.record().clone();
        let candidate = state::Id::from(predecessor.candidate.id.unwrap());
        fixture
            .database
            .clear_transition_if_matches(candidate, &predecessor.transition_id)
            .unwrap();
        let failure = ready.begin().unwrap_err();
        assert_usr_exchange_preflight_failure(failure, &predecessor.transition_id, predecessor.phase);
        assert_eq!(read_canonical(&fixture.installation.root), predecessor);
    }
}

#[test]
fn journal_coordinator_usr_exchange_intent_reseals_candidate_before_advance() {
    let (fixture, ready) = coordinator_ready_for_usr_exchange(CandidateKind::NewState);
    let predecessor = ready.record().clone();
    let payload = fixture.candidate_path.join("payload-sentinel");
    let alias = fixture.candidate_path.join("payload-sentinel.intent-alias");
    fs::hard_link(&payload, &alias).unwrap();
    assert_eq!(fs::symlink_metadata(&payload).unwrap().nlink(), 2);

    let failure = ready.begin().unwrap_err();

    assert_usr_exchange_preflight_failure(failure, &predecessor.transition_id, predecessor.phase);
    assert_eq!(read_canonical(&fixture.installation.root), predecessor);
    assert!(alias.is_file());
}

#[test]
fn journal_coordinator_usr_exchange_intent_faults_leave_exact_predecessor_or_intent() {
    for candidate_kind in [
        CandidateKind::NewState,
        CandidateKind::Archived,
        CandidateKind::ActiveReblit,
    ] {
        let (operation, predecessor_phase, predecessor_generation, intent_generation) =
            expected_usr_exchange_predecessor(candidate_kind);
        for post_exchange in [false, true] {
            let (fixture, ready) = coordinator_ready_for_usr_exchange(candidate_kind);
            let transition_id = ready.record().transition_id.clone();
            if post_exchange {
                crate::transition_journal::arm_next_update_first_directory_sync_fault();
            } else {
                crate::transition_journal::arm_next_temporary_sync_fault();
            }

            let failure = ready.begin().unwrap_err();

            if post_exchange {
                crate::transition_journal::assert_update_first_directory_sync_fault_consumed();
            } else {
                crate::transition_journal::assert_temporary_sync_fault_consumed();
            }
            assert!(matches!(
                failure,
                UsrExchangeIntentFailure::IntentPersistence {
                    transition_id: actual_transition,
                    predecessor,
                    ..
                } if actual_transition == transition_id && predecessor == predecessor_phase
            ));
            let expected_phase = if post_exchange {
                Phase::UsrExchangeIntent
            } else {
                predecessor_phase
            };
            let expected_generation = if post_exchange {
                intent_generation
            } else {
                predecessor_generation
            };
            assert_record_prefix(
                &read_canonical(&fixture.installation.root),
                operation,
                expected_phase,
                expected_generation,
            );
        }
    }
}

#[test]
fn journal_coordinator_usr_exchange_intent_failure_releases_journal_while_error_lives() {
    let (fixture, ready) = coordinator_ready_for_usr_exchange(CandidateKind::Archived);
    let root = fixture.installation.root.clone();
    let predecessor = ready.record().clone();
    crate::transition_journal::arm_next_temporary_sync_fault();

    let failure = ready.begin().unwrap_err();

    crate::transition_journal::assert_temporary_sync_fault_consumed();
    assert!(matches!(
        failure,
        UsrExchangeIntentFailure::IntentPersistence {
            ref transition_id,
            predecessor: Phase::CandidatePrepared,
            ..
        } if *transition_id == predecessor.transition_id
    ));
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let record = TransitionJournalStore::open(&root).unwrap().load().unwrap().unwrap();
        sender.send((record.phase, record.generation)).unwrap();
    });
    assert_eq!(
        receiver.recv_timeout(std::time::Duration::from_secs(10)),
        Ok((Phase::CandidatePrepared, 3)),
        "a returned /usr exchange-intent failure retained the exclusive journal lock"
    );
    worker.join().unwrap();
    drop(failure);
}
