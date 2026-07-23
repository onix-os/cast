#[test]
fn bound_record_advance_until_rejects_initial_expiry_before_temporary_creation() {
    let (temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
    let next = legal_forward_advance(&current);
    let deadline = Instant::now();
    let expired = deadline + Duration::from_nanos(1);
    let mut clock = ScriptedBoundAdvanceDeadlineClock::new([expired, expired]);
    take_durability_checkpoints();

    let error = store
        .advance_record_binding_until_with_test_clock(&cast, binding, &next, deadline, &mut clock)
        .unwrap_err();

    assert_eq!(clock.samples(), 1);
    assert!(matches!(
        error,
        StorageError::BoundAdvanceDeadlineExceeded { deadline: observed } if observed == deadline
    ));
    assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), Some(current));
    assert!(take_durability_checkpoints().is_empty());
    assert_no_journal_temporaries(temporary.path());
}

#[test]
fn bound_record_advance_until_accepts_deadline_equality_at_both_boundaries() {
    let (_temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
    let next = legal_forward_advance(&current);
    let deadline = Instant::now();
    let mut clock = ScriptedBoundAdvanceDeadlineClock::new([deadline, deadline]);

    let successor = store
        .advance_record_binding_until_with_test_clock(&cast, binding, &next, deadline, &mut clock)
        .unwrap();

    assert_eq!(clock.samples(), 2);
    assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), Some(next.clone()));
    assert!(store.has_record_binding(&cast, &successor, &next).unwrap());
}

#[test]
fn bound_record_advance_until_durably_persists_exact_boot_sync_complete_receipt_binding() {
    let (temporary, store) = fixture();
    let mut started = creation_record();
    store.create(&started).unwrap();
    while started.phase != Phase::BootSyncStarted {
        let next = legal_forward_advance(&started);
        store.advance(&started, &next).unwrap();
        started = next;
    }
    assert_eq!(started.version, PAYLOAD_VERSION);
    let receipts = started
        .boot_publication_receipt_correlation()
        .unwrap()
        .expect("v3 boot-sync-started record carries receipt correlation");
    let complete = started.boot_sync_complete_successor(receipts).unwrap();
    let cast = fs::File::open(temporary.path().join(".cast")).unwrap();
    let binding = store.record_binding(&cast, &started).unwrap();
    let deadline = Instant::now();
    let mut clock = ScriptedBoundAdvanceDeadlineClock::new([deadline, deadline]);
    take_durability_checkpoints();

    let successor = store
        .advance_record_binding_until_with_test_clock(
            &cast,
            binding,
            &complete,
            deadline,
            &mut clock,
        )
        .unwrap();

    assert_eq!(
        take_durability_checkpoints(),
        [
            DurabilityCheckpoint::TemporaryFullySynced,
            DurabilityCheckpoint::CanonicalExchanged,
            DurabilityCheckpoint::JournalDirectorySynced,
            DurabilityCheckpoint::DisplacedUnlinked,
            DurabilityCheckpoint::JournalDirectorySynced,
        ]
    );
    assert_eq!(complete.phase, Phase::BootSyncComplete);
    assert_eq!(
        complete.boot_publication_receipt_correlation().unwrap(),
        Some(receipts)
    );
    assert!(store.has_record_binding(&cast, &successor, &complete).unwrap());
    drop(store);

    let reopened = TransitionJournalStore::open_in_retained_cast(&cast, temporary.path()).unwrap();
    let persisted = reopened
        .load_revalidated_retained_cast(&cast)
        .unwrap()
        .expect("durable boot-sync-complete record");
    assert_eq!(persisted, complete);
    assert_eq!(
        persisted.boot_publication_receipt_correlation().unwrap(),
        Some(receipts)
    );
    assert!(
        reopened
            .has_reopened_record_binding(&cast, &successor, &persisted)
            .unwrap()
    );
}

#[test]
fn bound_record_advance_until_post_temporary_expiry_removes_only_its_temporary() {
    let (temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
    let next = legal_forward_advance(&current);
    let deadline = Instant::now();
    let expired = deadline + Duration::from_nanos(1);
    let mut clock = ScriptedBoundAdvanceDeadlineClock::new([deadline, expired]);
    take_durability_checkpoints();

    let error = store
        .advance_record_binding_until_with_test_clock(&cast, binding, &next, deadline, &mut clock)
        .unwrap_err();

    assert_eq!(clock.samples(), 2);
    assert!(matches!(
        error,
        StorageError::BoundAdvanceDeadlineExceeded { deadline: observed } if observed == deadline
    ));
    assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), Some(current));
    assert_no_journal_temporaries(temporary.path());
    assert_eq!(
        take_durability_checkpoints(),
        [
            DurabilityCheckpoint::TemporaryFullySynced,
            DurabilityCheckpoint::JournalDirectorySynced,
        ]
    );
}

#[test]
fn bound_record_advance_until_expiry_never_unlinks_a_substituted_temporary_name() {
    let (temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
    let next = legal_forward_advance(&current);
    let next_bytes = encode(&next).unwrap();
    let deadline = Instant::now();
    let expired = deadline + Duration::from_nanos(1);
    let journal = temporary.path().join(".cast/journal");
    let displaced = temporary.path().join("deadline-temporary-displaced");
    let substituted_path = std::rc::Rc::new(std::cell::RefCell::new(None));
    let callback_substituted_path = std::rc::Rc::clone(&substituted_path);
    let callback_displaced = displaced.clone();
    arm_bound_advance_before_expired_cleanup_callback(move |temporary_name| {
        assert!(valid_temporary_name(temporary_name.as_bytes()));
        let temporary_path = journal.join(std::ffi::OsStr::from_bytes(temporary_name.as_bytes()));
        fs::rename(&temporary_path, &callback_displaced).unwrap();
        fs::write(&temporary_path, b"foreign-temporary-winner").unwrap();
        fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o600)).unwrap();
        callback_substituted_path.replace(Some(temporary_path));
    });
    let mut clock = ScriptedBoundAdvanceDeadlineClock::new([deadline, expired]);

    let error = store
        .advance_record_binding_until_with_test_clock(&cast, binding, &next, deadline, &mut clock)
        .unwrap_err();

    assert_bound_advance_before_expired_cleanup_callback_consumed();
    assert_eq!(clock.samples(), 2);
    assert!(matches!(error, StorageError::CanonicalChanged));
    let substituted_path = std::rc::Rc::try_unwrap(substituted_path)
        .unwrap()
        .into_inner()
        .unwrap();
    assert_eq!(fs::read(&substituted_path).unwrap(), b"foreign-temporary-winner");
    assert_eq!(fs::read(&displaced).unwrap(), next_bytes);
    let substituted = fs::symlink_metadata(substituted_path).unwrap();
    let exact = fs::symlink_metadata(displaced).unwrap();
    assert_ne!((substituted.dev(), substituted.ino()), (exact.dev(), exact.ino()));
    assert_eq!(store.load().unwrap(), Some(current));
}

#[test]
fn bound_record_advance_until_allowed_time_reauth_rejects_a_substituted_temporary() {
    let (temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
    let next = legal_forward_advance(&current);
    let next_bytes = encode(&next).unwrap();
    let deadline = Instant::now();
    let journal = temporary.path().join(".cast/journal");
    let displaced = temporary.path().join("allowed-time-temporary-displaced");
    let substituted_path = std::rc::Rc::new(std::cell::RefCell::new(None));
    let callback_substituted_path = std::rc::Rc::clone(&substituted_path);
    let callback_displaced = displaced.clone();
    arm_bound_advance_before_final_deadline_callback(move |temporary_name| {
        assert!(valid_temporary_name(temporary_name.as_bytes()));
        let temporary_path = journal.join(std::ffi::OsStr::from_bytes(temporary_name.as_bytes()));
        fs::rename(&temporary_path, &callback_displaced).unwrap();
        fs::write(&temporary_path, b"foreign-allowed-time-winner").unwrap();
        fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o600)).unwrap();
        callback_substituted_path.replace(Some(temporary_path));
    });
    let mut clock = ScriptedBoundAdvanceDeadlineClock::new([deadline, deadline]);
    take_durability_checkpoints();

    let error = store
        .advance_record_binding_until_with_test_clock(&cast, binding, &next, deadline, &mut clock)
        .unwrap_err();

    assert_bound_advance_before_final_deadline_callback_consumed();
    assert_eq!(clock.samples(), 1);
    assert!(matches!(error, StorageError::CanonicalChanged));
    assert_eq!(store.load().unwrap(), Some(current));
    assert_eq!(take_durability_checkpoints(), [DurabilityCheckpoint::TemporaryFullySynced]);
    let substituted_path = std::rc::Rc::try_unwrap(substituted_path)
        .unwrap()
        .into_inner()
        .unwrap();
    assert_eq!(fs::read(&substituted_path).unwrap(), b"foreign-allowed-time-winner");
    assert_eq!(fs::read(&displaced).unwrap(), next_bytes);
    let substituted = fs::symlink_metadata(substituted_path).unwrap();
    let exact = fs::symlink_metadata(displaced).unwrap();
    assert_ne!((substituted.dev(), substituted.ino()), (exact.dev(), exact.ino()));
}

#[test]
fn bound_record_advance_until_expiry_cleanup_faults_fail_stop_with_exact_storage_errors() {
    let mut cases = 0;
    for (point, temporary_present) in [
        (StorageFaultPoint::BoundAdvanceDeadlineCleanupUnlink, true),
        (
            StorageFaultPoint::BoundAdvanceDeadlineCleanupDirectorySync,
            false,
        ),
    ] {
        let (temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
        let next = legal_forward_advance(&current);
        let deadline = Instant::now();
        let expired = deadline + Duration::from_nanos(1);
        let mut clock = ScriptedBoundAdvanceDeadlineClock::new([deadline, expired]);
        arm_storage_fault(point);

        let error = store
            .advance_record_binding_until_with_test_clock(&cast, binding, &next, deadline, &mut clock)
            .unwrap_err();

        assert_storage_fault_consumed();
        assert_eq!(clock.samples(), 2);
        assert!(match point {
            StorageFaultPoint::BoundAdvanceDeadlineCleanupUnlink => {
                matches!(error, StorageError::CleanupTemporary { .. })
            }
            StorageFaultPoint::BoundAdvanceDeadlineCleanupDirectorySync => {
                matches!(error, StorageError::SyncJournalDirectory { .. })
            }
            _ => unreachable!(),
        });
        assert_eq!(store.load().unwrap(), Some(current));
        let present = fs::read_dir(temporary.path().join(".cast/journal"))
            .unwrap()
            .any(|entry| valid_temporary_name(entry.unwrap().file_name().as_bytes()));
        assert_eq!(present, temporary_present);
        cases += 1;
    }
    assert_eq!(cases, 2, "deadline cleanup storage-fault matrix drifted");
}

#[test]
fn bound_record_advance_until_publication_faults_preserve_predecessor_or_successor() {
    let mut cases = 0;
    for (point, successor_visible) in [
        (StorageFaultPoint::TemporarySync, false),
        (StorageFaultPoint::UpdateExchange, false),
        (StorageFaultPoint::UpdateFirstDirectorySync, true),
        (StorageFaultPoint::DisplacedUnlink, true),
        (StorageFaultPoint::UpdateFinalDirectorySync, true),
    ] {
        let (_temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
        let next = legal_forward_advance(&current);
        let deadline = Instant::now();
        let mut clock = ScriptedBoundAdvanceDeadlineClock::new([deadline, deadline]);
        arm_storage_fault(point);

        let error = store
            .advance_record_binding_until_with_test_clock(&cast, binding, &next, deadline, &mut clock)
            .unwrap_err();

        assert_storage_fault_consumed();
        assert!(matches!(
            error,
            StorageError::SyncTemporary { .. }
                | StorageError::PublishCanonical { .. }
                | StorageError::SyncJournalDirectory { .. }
                | StorageError::DeleteDisplaced { .. }
        ));
        assert_eq!(store.load().unwrap(), Some(if successor_visible { next } else { current }));
        cases += 1;
    }
    assert_eq!(cases, 5, "deadline-bound publication fault matrix drifted");
}
