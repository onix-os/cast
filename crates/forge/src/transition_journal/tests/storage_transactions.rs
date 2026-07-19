use std::sync::Mutex;

#[test]
fn journal_directory_and_canonical_file_have_exact_private_metadata() {
    let (temporary, store) = fixture();
    store.create(&creation_record()).unwrap();
    let journal = temporary.path().join(".cast/journal");
    let journal_metadata = fs::metadata(journal).unwrap();
    let canonical_metadata = fs::metadata(canonical(temporary.path())).unwrap();
    let lock_metadata = fs::metadata(temporary.path().join(".cast/journal/state-transition.lock")).unwrap();
    assert_eq!(journal_metadata.permissions().mode() & 0o7777, 0o700);
    assert_eq!(canonical_metadata.permissions().mode() & 0o7777, 0o600);
    assert_eq!(canonical_metadata.nlink(), 1);
    assert!(canonical_metadata.file_type().is_file());
    assert_eq!(lock_metadata.permissions().mode() & 0o7777, 0o600);
    assert_eq!(lock_metadata.nlink(), 1);
}

#[test]
fn restrictive_umask_structural_residue_is_normalized_by_pinned_inode() {
    let temporary = tempfile::tempdir().unwrap();
    let cast = temporary.path().join(".cast");
    let journal = cast.join("journal");
    let lock = journal.join("state-transition.lock");
    fs::create_dir(&cast).unwrap();
    fs::set_permissions(&cast, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(&journal).unwrap();
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o000)).unwrap();

    let store = TransitionJournalStore::open(temporary.path()).unwrap();
    assert_eq!(fs::metadata(&journal).unwrap().permissions().mode() & 0o7777, 0o700);
    assert_eq!(fs::metadata(&lock).unwrap().permissions().mode() & 0o7777, 0o600);
    drop(store);

    // Once normalized, the ordinary exact-mode reopen path remains valid.
    TransitionJournalStore::open(temporary.path()).unwrap();
}

#[test]
fn atomic_initial_update_and_delete_round_trip() {
    let (temporary, store) = fixture();
    assert!(store.load().unwrap().is_none());
    let mut first = archived_record(Phase::Preparing);
    first.version = PAYLOAD_VERSION_V1;
    first.generation = 1;
    store.create(&first).unwrap();
    assert_eq!(store.load().unwrap(), Some(first.clone()));

    let rollback = satisfied_preparing_rollback(&first);
    assert_eq!(rollback.version, PAYLOAD_VERSION_V1);
    store.advance(&first, &rollback).unwrap();
    let complete = advance_record(&rollback, Phase::RollbackComplete);
    store.advance(&rollback, &complete).unwrap();
    let mut names = fs::read_dir(temporary.path().join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["state-transition", "state-transition.lock"]);

    assert!(store.delete(&complete).unwrap());
    assert!(!store.delete(&complete).unwrap());
    assert!(store.load().unwrap().is_none());
}

#[test]
fn journal_update_durability_callbacks_follow_filesystem_operation_order() {
    let (_temporary, store) = fixture();
    let initial = creation_record();
    store.create(&initial).unwrap();

    let observed = Arc::new(Mutex::new(Vec::new()));
    let temporary_observed = Arc::clone(&observed);
    arm_journal_update_durability_callback(
        JournalUpdateDurabilityBoundary::TemporaryFullySynced,
        move || {
            temporary_observed
                .lock()
                .unwrap()
                .push(JournalUpdateDurabilityBoundary::TemporaryFullySynced);
            let exchange_observed = Arc::clone(&temporary_observed);
            arm_journal_update_durability_callback(
                JournalUpdateDurabilityBoundary::CanonicalExchanged,
                move || {
                    exchange_observed
                        .lock()
                        .unwrap()
                        .push(JournalUpdateDurabilityBoundary::CanonicalExchanged);
                    let first_sync_observed = Arc::clone(&exchange_observed);
                    arm_journal_update_durability_callback(
                        JournalUpdateDurabilityBoundary::UpdateFirstDirectorySynced,
                        move || {
                            first_sync_observed
                                .lock()
                                .unwrap()
                                .push(JournalUpdateDurabilityBoundary::UpdateFirstDirectorySynced);
                            let unlink_observed = Arc::clone(&first_sync_observed);
                            arm_journal_update_durability_callback(
                                JournalUpdateDurabilityBoundary::DisplacedUnlinked,
                                move || {
                                    unlink_observed
                                        .lock()
                                        .unwrap()
                                        .push(JournalUpdateDurabilityBoundary::DisplacedUnlinked);
                                    let final_sync_observed = Arc::clone(&unlink_observed);
                                    arm_journal_update_durability_callback(
                                        JournalUpdateDurabilityBoundary::UpdateFinalDirectorySynced,
                                        move || {
                                            final_sync_observed.lock().unwrap().push(
                                                JournalUpdateDurabilityBoundary::UpdateFinalDirectorySynced,
                                            );
                                        },
                                    );
                                },
                            );
                        },
                    );
                },
            );
        },
    );

    let next = advance_record(&initial, Phase::FreshStateAllocating);
    store.advance(&initial, &next).unwrap();
    assert_eq!(
        *observed.lock().unwrap(),
        [
            JournalUpdateDurabilityBoundary::TemporaryFullySynced,
            JournalUpdateDurabilityBoundary::CanonicalExchanged,
            JournalUpdateDurabilityBoundary::UpdateFirstDirectorySynced,
            JournalUpdateDurabilityBoundary::DisplacedUnlinked,
            JournalUpdateDurabilityBoundary::UpdateFinalDirectorySynced,
        ]
    );
}

#[test]
fn journal_delete_durability_callbacks_follow_filesystem_operation_order() {
    let (_temporary, store) = fixture();
    let initial = creation_record();
    store.create(&initial).unwrap();
    let terminal = advance_to_complete(&store, initial);

    let observed = Arc::new(Mutex::new(Vec::new()));
    let unlink_observed = Arc::clone(&observed);
    arm_journal_delete_durability_callback(JournalDeleteDurabilityBoundary::CanonicalUnlinked, move || {
        unlink_observed
            .lock()
            .unwrap()
            .push(JournalDeleteDurabilityBoundary::CanonicalUnlinked);
        let sync_observed = Arc::clone(&unlink_observed);
        arm_journal_delete_durability_callback(
            JournalDeleteDurabilityBoundary::DeleteDirectorySynced,
            move || {
                sync_observed
                    .lock()
                    .unwrap()
                    .push(JournalDeleteDurabilityBoundary::DeleteDirectorySynced);
            },
        );
    });

    assert!(store.delete(&terminal).unwrap());
    assert_eq!(
        *observed.lock().unwrap(),
        [
            JournalDeleteDurabilityBoundary::CanonicalUnlinked,
            JournalDeleteDurabilityBoundary::DeleteDirectorySynced,
        ]
    );
    assert!(!store.delete(&terminal).unwrap());
    assert_eq!(observed.lock().unwrap().len(), 2, "delete callbacks must remain one-shot");
}

#[test]
fn temporary_update_callback_survives_create_and_is_one_shot() {
    let (_temporary, store) = fixture();
    let observed = Arc::new(Mutex::new(0));
    let callback_observed = Arc::clone(&observed);
    arm_journal_update_durability_callback(
        JournalUpdateDurabilityBoundary::TemporaryFullySynced,
        move || *callback_observed.lock().unwrap() += 1,
    );

    let initial = creation_record();
    store.create(&initial).unwrap();
    assert_eq!(*observed.lock().unwrap(), 0, "create must not consume an update seam");

    let allocating = advance_record(&initial, Phase::FreshStateAllocating);
    store.advance(&initial, &allocating).unwrap();
    assert_eq!(*observed.lock().unwrap(), 1);

    let allocated = advance_record(&allocating, Phase::FreshStateAllocated);
    store.advance(&allocating, &allocated).unwrap();
    assert_eq!(*observed.lock().unwrap(), 1, "the callback must remain one-shot");
}

#[test]
fn create_advance_and_delete_are_exactly_conditional() {
    let (_temporary, store) = fixture();
    let initial = creation_record();
    store.create(&initial).unwrap();
    assert!(matches!(
        store.create(&initial),
        Err(StorageError::CanonicalAlreadyExists)
    ));

    let mut foreign = creation_record();
    foreign.transition_id = other_id();
    let foreign_next = advance_record(&foreign, Phase::FreshStateAllocating);
    assert!(matches!(
        store.advance(&foreign, &foreign_next),
        Err(StorageError::ExpectedRecordMismatch)
    ));
    assert_eq!(store.load().unwrap(), Some(initial));
}

#[test]
fn one_shared_store_serializes_competing_compare_and_swap_advances() {
    let (_temporary, store) = fixture();
    let initial = creation_record();
    let allocating = advance_record(&initial, Phase::FreshStateAllocating);
    store.create(&initial).unwrap();
    store.advance(&initial, &allocating).unwrap();

    let first = advance_record(&allocating, Phase::FreshStateAllocated);
    let mut second = first.clone();
    second.candidate.id = Some(43);
    let store = Arc::new(store);
    let held = store.operation_lock.lock().unwrap();
    let (attempted_tx, attempted_rx) = mpsc::channel();
    let mut workers = Vec::new();
    for proposal in [first.clone(), second.clone()] {
        let store = Arc::clone(&store);
        let expected = allocating.clone();
        let attempted = attempted_tx.clone();
        workers.push(thread::spawn(move || {
            attempted.send(()).unwrap();
            let result = store.advance(&expected, &proposal);
            (proposal, result)
        }));
    }
    drop(attempted_tx);
    attempted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    attempted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(held);

    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|(_, result)| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|(_, result)| matches!(result, Err(StorageError::ExpectedRecordMismatch)))
            .count(),
        1
    );
    let winner = results
        .into_iter()
        .find_map(|(proposal, result)| result.is_ok().then_some(proposal))
        .unwrap();
    assert_eq!(store.load().unwrap(), Some(winner));
}

#[test]
fn internal_lock_serializes_stale_thread_writers() {
    let (temporary, store) = fixture();
    let initial = creation_record();
    let next = advance_record(&initial, Phase::FreshStateAllocating);
    store.create(&initial).unwrap();

    let root = temporary.path().to_owned();
    let thread_initial = initial.clone();
    let thread_next = next.clone();
    let (attempted_tx, attempted_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        attempted_tx.send(()).unwrap();
        let competing = TransitionJournalStore::open(&root).unwrap();
        let stale_rejected = matches!(
            competing.advance(&thread_initial, &thread_next),
            Err(StorageError::ExpectedRecordMismatch)
        );
        finished_tx.send(stale_rejected).unwrap();
    });

    attempted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        finished_rx.recv_timeout(Duration::from_millis(150)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    store.advance(&initial, &next).unwrap();
    drop(store);
    assert!(finished_rx.recv_timeout(Duration::from_secs(5)).unwrap());
    worker.join().unwrap();

    let store = TransitionJournalStore::open(temporary.path()).unwrap();
    assert_eq!(store.load().unwrap(), Some(next));
}

#[test]
fn internal_lock_prevents_stale_writer_resurrection_after_delete() {
    let (temporary, store) = fixture();
    let initial = creation_record();
    store.create(&initial).unwrap();
    let mut preterminal = initial;
    while preterminal.phase != Phase::CommitCleanupComplete {
        let next = legal_forward_advance(&preterminal);
        store.advance(&preterminal, &next).unwrap();
        preterminal = next;
    }
    let terminal = legal_forward_advance(&preterminal);

    let root = temporary.path().to_owned();
    let thread_expected = preterminal.clone();
    let thread_terminal = terminal.clone();
    let (attempted_tx, attempted_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        attempted_tx.send(()).unwrap();
        let competing = TransitionJournalStore::open(&root).unwrap();
        matches!(
            competing.advance(&thread_expected, &thread_terminal),
            Err(StorageError::CanonicalMissing)
        )
    });
    attempted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    store.advance(&preterminal, &terminal).unwrap();
    assert!(store.delete(&terminal).unwrap());
    drop(store);
    assert!(worker.join().unwrap());

    let store = TransitionJournalStore::open(temporary.path()).unwrap();
    assert!(store.load().unwrap().is_none());
}

#[test]
fn exclusive_lock_serializes_subprocess_open() {
    const CHILD: &str = "CAST_TRANSITION_JOURNAL_LOCK_CHILD";
    const ROOT: &str = "CAST_TRANSITION_JOURNAL_LOCK_ROOT";
    const STARTED: &str = "CAST_TRANSITION_JOURNAL_LOCK_STARTED";
    const ACQUIRED: &str = "CAST_TRANSITION_JOURNAL_LOCK_ACQUIRED";
    const TEST: &str = "transition_journal::tests::exclusive_lock_serializes_subprocess_open";

    if std::env::var_os(CHILD).is_some() {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        let started = PathBuf::from(std::env::var_os(STARTED).unwrap());
        let acquired = PathBuf::from(std::env::var_os(ACQUIRED).unwrap());
        fs::write(started, b"started").unwrap();
        let _store = TransitionJournalStore::open(&root).unwrap();
        fs::write(acquired, b"acquired").unwrap();
        return;
    }

    let (temporary, store) = fixture();
    let started = temporary.path().join("child-started");
    let acquired = temporary.path().join("child-acquired");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg(TEST)
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD, "1")
        .env(ROOT, temporary.path())
        .env(STARTED, &started)
        .env(ACQUIRED, &acquired)
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        started.exists(),
        "subprocess never attempted to acquire the journal lock"
    );
    thread::sleep(Duration::from_millis(150));
    assert!(!acquired.exists());
    assert!(child.try_wait().unwrap().is_none());
    drop(store);
    let status = child.wait().unwrap();
    assert!(status.success());
    assert!(acquired.exists());
}
