#[test]
fn stale_pid_reuse_temporaries_over_old_retry_limit_are_cleaned_boundedly() {
    let (temporary, store) = fixture();
    create_stale_temporaries(temporary.path(), 160);
    drop(store);

    let store = TransitionJournalStore::open(temporary.path()).unwrap();
    let names = fs::read_dir(temporary.path().join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, ["state-transition.lock"]);
    let temporary_record = store.create_temporary().unwrap();
    store.cleanup_temporary(&temporary_record).unwrap();
}

#[test]
fn stale_temporary_cleanup_has_a_hard_cap_and_never_partially_cleans() {
    let (temporary, store) = fixture();
    store.create(&creation_record()).unwrap();
    create_stale_temporaries(temporary.path(), MAX_STALE_TEMPORARIES + 1);
    drop(store);

    assert!(matches!(
        TransitionJournalStore::open(temporary.path()),
        Err(StorageError::TooManyStaleTemporaries)
    ));
    let stale = fs::read_dir(temporary.path().join(".cast/journal"))
        .unwrap()
        .filter(|entry| valid_temporary_name(entry.as_ref().unwrap().file_name().as_bytes()))
        .count();
    assert_eq!(stale, MAX_STALE_TEMPORARIES + 1);
}

#[test]
fn unsafe_stale_temporary_is_preserved_and_never_followed() {
    let (temporary, store) = fixture();
    let target = temporary.path().join("outside-target");
    fs::write(&target, b"outside").unwrap();
    let stale = stale_temporary_path(temporary.path(), 9);
    symlink(&target, &stale).unwrap();
    drop(store);

    assert!(matches!(
        TransitionJournalStore::open(temporary.path()),
        Err(StorageError::ValidateStaleTemporary { .. })
    ));
    assert_eq!(fs::read(target).unwrap(), b"outside");
    assert!(fs::symlink_metadata(stale).unwrap().file_type().is_symlink());
}

#[test]
fn crash_before_publish_keeps_old_canonical_and_cleans_temp() {
    let (temporary, store) = fixture();
    let initial = creation_record();
    let next = advance_record(&initial, Phase::FreshStateAllocating);
    store.create(&initial).unwrap();
    let mut pending = store.create_temporary().unwrap();
    pending.file.write_all(&encode(&next).unwrap()).unwrap();
    pending.file.sync_all().unwrap();
    drop(pending);
    drop(store);

    let store = TransitionJournalStore::open(temporary.path()).unwrap();
    assert_eq!(store.load().unwrap(), Some(initial));
    assert_eq!(fs::read_dir(temporary.path().join(".cast/journal")).unwrap().count(), 2);
}

#[test]
fn crash_with_a_partial_temporary_never_promotes_it() {
    let (temporary, store) = fixture();
    let initial = creation_record();
    let next = advance_record(&initial, Phase::FreshStateAllocating);
    store.create(&initial).unwrap();
    let mut pending = store.create_temporary().unwrap();
    let framed = encode(&next).unwrap();
    pending.file.write_all(&framed[..framed.len() / 2]).unwrap();
    drop(pending);
    drop(store);

    let store = TransitionJournalStore::open(temporary.path()).unwrap();
    assert_eq!(store.load().unwrap(), Some(initial));
    assert_no_journal_temporaries(temporary.path());
}

#[test]
fn crash_after_exchange_keeps_new_canonical_and_cleans_displaced_record() {
    let (temporary, store) = fixture();
    let initial = creation_record();
    let next = advance_record(&initial, Phase::FreshStateAllocating);
    store.create(&initial).unwrap();
    let mut pending = store.create_temporary().unwrap();
    pending.file.write_all(&encode(&next).unwrap()).unwrap();
    pending.file.sync_all().unwrap();
    renameat2(
        store.directory.as_raw_fd(),
        &pending.name,
        store.directory.as_raw_fd(),
        CANONICAL_NAME,
        nix::libc::RENAME_EXCHANGE,
    )
    .unwrap();
    drop(pending);
    drop(store);

    let store = TransitionJournalStore::open(temporary.path()).unwrap();
    assert_eq!(store.load().unwrap(), Some(next));
    assert_eq!(fs::read_dir(temporary.path().join(".cast/journal")).unwrap().count(), 2);
}

#[test]
fn injected_create_publish_faults_reopen_to_absent_or_exact_new_record() {
    for (point, published) in [
        (StorageFaultPoint::TemporarySync, false),
        (StorageFaultPoint::InitialRename, false),
        (StorageFaultPoint::InitialDirectorySync, true),
    ] {
        let (temporary, store) = fixture();
        let sentinel = temporary.path().join("outside-sentinel");
        fs::write(&sentinel, b"foreign").unwrap();
        let record = creation_record();
        arm_storage_fault(point);
        assert!(store.create(&record).is_err(), "fault {point:?} unexpectedly succeeded");
        assert_storage_fault_consumed();
        drop(store);

        let reopened = TransitionJournalStore::open(temporary.path()).unwrap();
        assert_eq!(reopened.load().unwrap(), published.then_some(record));
        assert_no_journal_temporaries(temporary.path());
        assert_eq!(fs::read(&sentinel).unwrap(), b"foreign");
    }
}

#[test]
fn injected_update_publish_faults_reopen_to_exact_old_or_new_record() {
    for (point, published_new) in [
        (StorageFaultPoint::TemporarySync, false),
        (StorageFaultPoint::UpdateExchange, false),
        (StorageFaultPoint::UpdateFirstDirectorySync, true),
        (StorageFaultPoint::DisplacedUnlink, true),
        (StorageFaultPoint::UpdateFinalDirectorySync, true),
    ] {
        let (temporary, store) = fixture();
        let sentinel = temporary.path().join("outside-sentinel");
        fs::write(&sentinel, b"foreign").unwrap();
        let old = creation_record();
        let new = advance_record(&old, Phase::FreshStateAllocating);
        store.create(&old).unwrap();
        arm_storage_fault(point);
        assert!(
            store.advance(&old, &new).is_err(),
            "fault {point:?} unexpectedly succeeded"
        );
        assert_storage_fault_consumed();
        drop(store);

        let reopened = TransitionJournalStore::open(temporary.path()).unwrap();
        assert_eq!(reopened.load().unwrap(), Some(if published_new { new } else { old }));
        assert_no_journal_temporaries(temporary.path());
        assert_eq!(fs::read(&sentinel).unwrap(), b"foreign");
    }
}

#[test]
fn injected_delete_faults_reopen_to_exact_terminal_or_absence() {
    for (point, deleted) in [
        (StorageFaultPoint::CanonicalUnlink, false),
        (StorageFaultPoint::DeleteDirectorySync, true),
    ] {
        let (temporary, store) = fixture();
        let sentinel = temporary.path().join("outside-sentinel");
        fs::write(&sentinel, b"foreign").unwrap();
        let mut initial = archived_record(Phase::Preparing);
        initial.generation = 1;
        store.create(&initial).unwrap();
        let rollback = satisfied_preparing_rollback(&initial);
        store.advance(&initial, &rollback).unwrap();
        let terminal = advance_record(&rollback, Phase::RollbackComplete);
        store.advance(&rollback, &terminal).unwrap();

        arm_storage_fault(point);
        assert!(
            store.delete(&terminal).is_err(),
            "fault {point:?} unexpectedly succeeded"
        );
        assert_storage_fault_consumed();
        drop(store);

        let reopened = TransitionJournalStore::open(temporary.path()).unwrap();
        assert_eq!(reopened.load().unwrap(), (!deleted).then_some(terminal));
        assert_no_journal_temporaries(temporary.path());
        assert_eq!(fs::read(&sentinel).unwrap(), b"foreign");
    }
}

#[test]
fn durability_checkpoints_prove_fsync_order_for_create_update_and_delete() {
    let (_temporary, store) = fixture();
    take_durability_checkpoints();
    let initial = creation_record();
    store.create(&initial).unwrap();
    assert_eq!(
        take_durability_checkpoints(),
        [
            DurabilityCheckpoint::TemporaryFullySynced,
            DurabilityCheckpoint::CanonicalPublished,
            DurabilityCheckpoint::JournalDirectorySynced,
        ]
    );

    let next = advance_record(&initial, Phase::FreshStateAllocating);
    store.advance(&initial, &next).unwrap();
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

    let terminal = advance_to_complete(&store, next);
    take_durability_checkpoints();
    store.delete(&terminal).unwrap();
    assert_eq!(
        take_durability_checkpoints(),
        [
            DurabilityCheckpoint::CanonicalUnlinked,
            DurabilityCheckpoint::JournalDirectorySynced,
        ]
    );
}

#[test]
fn corrupt_canonical_is_never_replaced_or_recovered_from_temporary() {
    let (temporary, store) = fixture();
    let canonical = canonical(temporary.path());
    fs::write(&canonical, b"corrupt").unwrap();
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
    let fallback = temporary.path().join(".cast/journal/.state-transition.tmp-fallback");
    fs::write(&fallback, encode(&record(Phase::Preparing)).unwrap()).unwrap();
    fs::set_permissions(&fallback, fs::Permissions::from_mode(0o600)).unwrap();

    assert!(matches!(store.load(), Err(StorageError::Decode(_))));
    assert!(matches!(store.create(&creation_record()), Err(StorageError::Decode(_))));
    assert!(matches!(
        store.delete(&record(Phase::RollbackComplete)),
        Err(StorageError::Decode(_))
    ));
    assert_eq!(fs::read(canonical).unwrap(), b"corrupt");
    assert!(fallback.exists());
}

#[test]
fn absent_canonical_never_promotes_a_valid_temporary() {
    let (temporary, store) = fixture();
    let fallback = temporary.path().join(".cast/journal/.state-transition.tmp-fallback");
    fs::write(&fallback, encode(&record(Phase::Preparing)).unwrap()).unwrap();
    fs::set_permissions(&fallback, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(store.load().unwrap().is_none());
}

#[test]
fn canonical_file_reader_enforces_the_exact_n_and_n_plus_one_boundary() {
    let (temporary, store) = fixture();
    let canonical = canonical(temporary.path());
    fs::write(&canonical, vec![0; MAX_CANONICAL_RECORD_BYTES]).unwrap();
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        store.load(),
        Err(StorageError::Decode(CodecError::InvalidMagic))
    ));

    fs::write(&canonical, vec![0; MAX_CANONICAL_RECORD_BYTES + 1]).unwrap();
    assert!(matches!(store.load(), Err(StorageError::ReadCanonical { .. })));
}

#[test]
fn temporary_files_are_exclusive_unique_and_private() {
    let (temporary, store) = fixture();
    let first = store.create_temporary().unwrap();
    let second = store.create_temporary().unwrap();
    assert_ne!(first.name, second.name);
    for entry in [&first, &second] {
        let metadata = entry.file.metadata().unwrap();
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert!(metadata.file_type().is_file());
    }
    store.cleanup_temporary(&first).unwrap();
    store.cleanup_temporary(&second).unwrap();
    let names = fs::read_dir(temporary.path().join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, ["state-transition.lock"]);
}

#[test]
fn canonical_symlink_is_rejected_without_touching_its_target() {
    let (temporary, store) = fixture();
    let target = temporary.path().join("target");
    fs::write(&target, b"outside").unwrap();
    symlink(&target, canonical(temporary.path())).unwrap();
    assert!(matches!(store.load(), Err(StorageError::OpenCanonical { .. })));
    assert!(store.create(&creation_record()).is_err());
    assert!(store.delete(&record(Phase::RollbackComplete)).is_err());
    assert_eq!(fs::read(target).unwrap(), b"outside");
}

#[test]
fn canonical_mode_and_hardlink_attacks_are_rejected() {
    let (temporary, store) = fixture();
    store.create(&creation_record()).unwrap();
    let canonical = canonical(temporary.path());
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(store.load(), Err(StorageError::ValidateCanonical { .. })));

    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&canonical, temporary.path().join("extra-link")).unwrap();
    assert!(matches!(store.load(), Err(StorageError::ValidateCanonical { .. })));
}

#[test]
fn canonical_wrong_inode_kind_is_rejected() {
    let (temporary, store) = fixture();
    fs::create_dir(canonical(temporary.path())).unwrap();
    assert!(store.load().is_err());
    assert!(store.create(&creation_record()).is_err());
    assert!(store.delete(&record(Phase::RollbackComplete)).is_err());
}

#[test]
fn journal_directory_symlink_and_mode_attacks_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let cast = temporary.path().join(".cast");
    fs::create_dir(&cast).unwrap();
    fs::set_permissions(&cast, fs::Permissions::from_mode(0o700)).unwrap();
    let target = temporary.path().join("target");
    fs::create_dir(&target).unwrap();
    let target_mode = fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
    symlink(&target, cast.join("journal")).unwrap();
    assert!(matches!(
        TransitionJournalStore::open(temporary.path()),
        Err(StorageError::OpenJournalDirectory { .. })
    ));
    assert!(
        fs::symlink_metadata(cast.join("journal"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
        target_mode
    );
    assert!(fs::read_dir(target).unwrap().next().is_none());

    fs::remove_file(cast.join("journal")).unwrap();
    fs::create_dir(cast.join("journal")).unwrap();
    fs::set_permissions(cast.join("journal"), fs::Permissions::from_mode(0o770)).unwrap();
    assert!(matches!(
        TransitionJournalStore::open(temporary.path()),
        Err(StorageError::OpenJournalDirectory { .. })
    ));
}

#[test]
fn cast_directory_symlink_is_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, temporary.path().join(".cast")).unwrap();
    assert!(matches!(
        TransitionJournalStore::open(temporary.path()),
        Err(StorageError::OpenCastDirectory { .. })
    ));
}

#[test]
fn internal_lock_symlink_mode_and_hardlink_attacks_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let cast = temporary.path().join(".cast");
    let journal = cast.join("journal");
    fs::create_dir(&cast).unwrap();
    fs::set_permissions(&cast, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(&journal).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
    let lock = journal.join("state-transition.lock");
    let target = temporary.path().join("outside-lock-target");
    fs::write(&target, b"outside").unwrap();
    symlink(&target, &lock).unwrap();
    assert!(matches!(
        TransitionJournalStore::open(temporary.path()),
        Err(StorageError::ValidateLock { .. })
    ));
    assert!(fs::symlink_metadata(&lock).unwrap().file_type().is_symlink());
    assert_eq!(fs::read(&target).unwrap(), b"outside");

    fs::remove_file(&lock).unwrap();
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        TransitionJournalStore::open(temporary.path()),
        Err(StorageError::ValidateLock { .. })
    ));
    assert_eq!(fs::metadata(&lock).unwrap().permissions().mode() & 0o7777, 0o644);

    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&lock, temporary.path().join("lock-hardlink")).unwrap();
    assert!(matches!(
        TransitionJournalStore::open(temporary.path()),
        Err(StorageError::ValidateLock { .. })
    ));
}

#[test]
fn metadata_validators_reject_wrong_owner_and_inode_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("file");
    fs::write(&path, b"record").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert!(require_safe_regular_file_metadata(&metadata, &path, metadata.uid().wrapping_add(1)).is_err());

    let other = temporary.path().join("other");
    fs::write(&other, b"record").unwrap();
    let first = fs::File::open(path).unwrap();
    let second = fs::File::open(other).unwrap();
    assert!(matches!(
        require_same_inode(inode_identity(&first).unwrap(), inode_identity(&second).unwrap()),
        Err(StorageError::CanonicalChanged)
    ));
}

#[test]
fn temporary_names_are_internal_single_bounded_components() {
    for _ in 0..256 {
        let name = temporary_name();
        let bytes = name.to_bytes();
        assert!(bytes.len() <= 255);
        assert!(!bytes.contains(&b'/'));
        assert!(bytes.starts_with(b".state-transition.tmp-"));
    }
}
