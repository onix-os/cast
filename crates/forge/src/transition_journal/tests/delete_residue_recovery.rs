const RECOVERY_RESIDUE_NAME: &str = ".state-transition.delete-deadbeef-0000000000000001";
const SECOND_RECOVERY_RESIDUE_NAME: &str = ".state-transition.delete-deadbeef-0000000000000002";

#[derive(Debug, Eq, PartialEq)]
struct DeleteResidueSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    accessed_seconds: i64,
    accessed_nanoseconds: i64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    bytes: Vec<u8>,
}

fn delete_residue_snapshot(path: &Path) -> DeleteResidueSnapshot {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOATIME | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(path)
        .unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    let metadata = fs::symlink_metadata(path).unwrap();
    DeleteResidueSnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        length: metadata.len(),
        accessed_seconds: metadata.atime(),
        accessed_nanoseconds: metadata.atime_nsec(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        bytes,
    }
}

fn assert_same_delete_residue_inode_and_frame(
    actual: &DeleteResidueSnapshot,
    expected: &DeleteResidueSnapshot,
) {
    assert_eq!(actual.device, expected.device);
    assert_eq!(actual.inode, expected.inode);
    assert_eq!(actual.mode, expected.mode);
    assert_eq!(actual.links, expected.links);
    assert_eq!(actual.length, expected.length);
    assert_eq!(actual.modified_seconds, expected.modified_seconds);
    assert_eq!(actual.modified_nanoseconds, expected.modified_nanoseconds);
    assert_eq!(actual.bytes, expected.bytes);
}

fn delete_recovery_journal_names(root: &Path) -> Vec<Vec<u8>> {
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            nix::libc::O_DIRECTORY
                | nix::libc::O_NOATIME
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW,
        )
        .open(root.join(".cast/journal"))
        .unwrap();
    let mut names = directory_entries(&directory)
        .unwrap()
        .into_iter()
        .map(|name| name.to_bytes().to_vec())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn prepare_terminal_delete_residue(
    terminal_phase: BoundDeleteTerminal,
) -> (tempfile::TempDir, TransitionRecord, PathBuf) {
    let (temporary, store, cast, terminal, binding) = bound_delete_terminal_fixture(terminal_phase);
    drop(binding);
    drop(store);
    drop(cast);
    let residue = temporary.path().join(".cast/journal").join(RECOVERY_RESIDUE_NAME);
    fs::rename(canonical(temporary.path()), &residue).unwrap();
    (temporary, terminal, residue)
}

fn prepare_nonterminal_delete_residue() -> (tempfile::TempDir, TransitionRecord, PathBuf) {
    let (temporary, store) = fixture();
    let record = creation_record();
    store.create(&record).unwrap();
    drop(store);
    let residue = temporary.path().join(".cast/journal").join(RECOVERY_RESIDUE_NAME);
    fs::rename(canonical(temporary.path()), &residue).unwrap();
    (temporary, record, residue)
}

#[test]
fn delete_residue_recovery_restores_genuine_bound_delete_residue_on_fresh_reopen() {
    let (temporary, store, cast, terminal, binding) = bound_delete_fixture();
    let blocker = temporary.path().join(".cast/journal/bound-delete-test-blocker");
    let callback_blocker = blocker.clone();
    arm_public_binding_revalidation_callback(
        PublicBindingRevalidationBoundary::BeforeBoundDeletePrivateUnlink,
        move || write_bound_delete_replacement(&callback_blocker, b"block private unlink"),
    );
    take_durability_checkpoints();
    let error = store.delete_record_binding(&cast, binding, &terminal).unwrap_err();
    assert_public_binding_revalidation_callback_consumed();
    assert!(matches!(
        error,
        TransitionJournalRecordDeleteError::Detached(StorageError::BoundDeleteEntrySetMismatch { .. })
    ));
    assert_eq!(take_durability_checkpoints(), [DurabilityCheckpoint::CanonicalDetached]);
    let residue = bound_delete_private_path(temporary.path());
    let retained = delete_residue_snapshot(&residue);
    fs::remove_file(blocker).unwrap();
    drop(store);
    drop(cast);

    let reopened = TransitionJournalStore::open(temporary.path()).unwrap();

    assert_eq!(reopened.load().unwrap(), Some(terminal));
    assert!(!residue.exists());
    assert_same_delete_residue_inode_and_frame(&delete_residue_snapshot(&canonical(temporary.path())), &retained);
}

#[test]
fn delete_residue_recovery_restores_both_deletable_terminal_phases_exactly() {
    let mut cases = 0;
    for terminal_phase in BoundDeleteTerminal::ALL {
        let (temporary, terminal, residue) = prepare_terminal_delete_residue(terminal_phase);
        let retained = delete_residue_snapshot(&residue);

        let reopened = TransitionJournalStore::open(temporary.path()).unwrap();

        assert_eq!(reopened.load().unwrap(), Some(terminal));
        assert_same_delete_residue_inode_and_frame(&delete_residue_snapshot(&canonical(temporary.path())), &retained);
        assert!(!residue.exists());
        assert_eq!(
            delete_recovery_journal_names(temporary.path()),
            [b"state-transition".to_vec(), b"state-transition.lock".to_vec()]
        );
        cases += 1;
    }
    assert_eq!(cases, 2, "delete-residue terminal phase matrix drifted");
}

#[test]
fn delete_residue_recovery_rejects_nonterminal_and_corrupt_frames_without_mutation() {
    let (nonterminal, record, residue) = prepare_nonterminal_delete_residue();
    assert!(!record.phase.deletable());
    let before = delete_residue_snapshot(&residue);
    let names = delete_recovery_journal_names(nonterminal.path());
    assert!(matches!(
        TransitionJournalStore::open(nonterminal.path()),
        Err(StorageError::NonterminalDeleteResidue { .. })
    ));
    assert_eq!(delete_residue_snapshot(&residue), before);
    assert_eq!(delete_recovery_journal_names(nonterminal.path()), names);

    let (corrupt, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    fs::write(&residue, b"corrupt interrupted deletion").unwrap();
    fs::set_permissions(&residue, fs::Permissions::from_mode(0o600)).unwrap();
    let before = delete_residue_snapshot(&residue);
    let names = delete_recovery_journal_names(corrupt.path());
    assert!(matches!(
        TransitionJournalStore::open(corrupt.path()),
        Err(StorageError::DecodeDeleteResidue { .. })
    ));
    assert_eq!(delete_residue_snapshot(&residue), before);
    assert_eq!(delete_recovery_journal_names(corrupt.path()), names);
}

#[test]
fn delete_residue_recovery_rejects_malformed_unsafe_and_foreign_inventory_without_mutation() {
    let (malformed, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let malformed_residue = residue.with_file_name(".state-transition.delete-DEADBEEF-0000000000000001");
    fs::rename(&residue, &malformed_residue).unwrap();
    let before = delete_residue_snapshot(&malformed_residue);
    assert!(matches!(
        TransitionJournalStore::open(malformed.path()),
        Err(StorageError::DeleteResidueEntrySetMismatch { .. })
    ));
    assert_eq!(delete_residue_snapshot(&malformed_residue), before);

    let (unsafe_mode, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    fs::set_permissions(&residue, fs::Permissions::from_mode(0o640)).unwrap();
    let before = delete_residue_snapshot(&residue);
    assert!(matches!(
        TransitionJournalStore::open(unsafe_mode.path()),
        Err(StorageError::ValidateDeleteResidue { .. })
    ));
    assert_eq!(delete_residue_snapshot(&residue), before);

    let (hardlinked, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let second_link = hardlinked.path().join("retained-delete-residue-link");
    fs::hard_link(&residue, &second_link).unwrap();
    let residue_before = delete_residue_snapshot(&residue);
    let link_before = delete_residue_snapshot(&second_link);
    assert!(matches!(
        TransitionJournalStore::open(hardlinked.path()),
        Err(StorageError::ValidateDeleteResidue { .. })
    ));
    assert_eq!(delete_residue_snapshot(&residue), residue_before);
    assert_eq!(delete_residue_snapshot(&second_link), link_before);

    let (symlinked, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let symlink_target = symlinked.path().join("retained-delete-residue-target");
    fs::rename(&residue, &symlink_target).unwrap();
    symlink(&symlink_target, &residue).unwrap();
    let target_before = delete_residue_snapshot(&symlink_target);
    let link_before = fs::symlink_metadata(&residue).unwrap();
    assert!(matches!(
        TransitionJournalStore::open(symlinked.path()),
        Err(StorageError::ValidateDeleteResidue { .. })
    ));
    let link_after = fs::symlink_metadata(&residue).unwrap();
    assert_eq!((link_after.dev(), link_after.ino(), link_after.mode()), (
        link_before.dev(), link_before.ino(), link_before.mode()
    ));
    assert_eq!(fs::read_link(&residue).unwrap(), symlink_target);
    assert_eq!(delete_residue_snapshot(&symlink_target), target_before);

    let (nonregular, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let retained_record = nonregular.path().join("retained-delete-residue-record");
    fs::rename(&residue, &retained_record).unwrap();
    fs::create_dir(&residue).unwrap();
    fs::set_permissions(&residue, fs::Permissions::from_mode(0o700)).unwrap();
    let record_before = delete_residue_snapshot(&retained_record);
    let directory_before = fs::symlink_metadata(&residue).unwrap();
    assert!(matches!(
        TransitionJournalStore::open(nonregular.path()),
        Err(StorageError::ValidateDeleteResidue { .. })
    ));
    let directory_after = fs::symlink_metadata(&residue).unwrap();
    assert_eq!((directory_after.dev(), directory_after.ino(), directory_after.mode()), (
        directory_before.dev(), directory_before.ino(), directory_before.mode()
    ));
    assert_eq!(delete_residue_snapshot(&retained_record), record_before);

    let (foreign, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let evidence = foreign.path().join(".cast/journal/foreign-evidence");
    fs::write(&evidence, b"foreign").unwrap();
    let residue_before = delete_residue_snapshot(&residue);
    let evidence_before = fs::read(&evidence).unwrap();
    let names = delete_recovery_journal_names(foreign.path());
    assert!(matches!(
        TransitionJournalStore::open(foreign.path()),
        Err(StorageError::DeleteResidueEntrySetMismatch { .. })
    ));
    assert_eq!(delete_residue_snapshot(&residue), residue_before);
    assert_eq!(fs::read(evidence).unwrap(), evidence_before);
    assert_eq!(delete_recovery_journal_names(foreign.path()), names);
}

#[test]
fn delete_residue_recovery_rejects_canonical_coexistence_and_multiple_residues() {
    let (coexistence, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    fs::copy(&residue, canonical(coexistence.path())).unwrap();
    let residue_before = delete_residue_snapshot(&residue);
    let canonical_before = delete_residue_snapshot(&canonical(coexistence.path()));
    assert!(matches!(
        TransitionJournalStore::open(coexistence.path()),
        Err(StorageError::DeleteResidueEntrySetMismatch { .. })
    ));
    assert_eq!(delete_residue_snapshot(&residue), residue_before);
    assert_eq!(delete_residue_snapshot(&canonical(coexistence.path())), canonical_before);

    let (multiple, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let second = residue.with_file_name(SECOND_RECOVERY_RESIDUE_NAME);
    fs::copy(&residue, &second).unwrap();
    let first_before = delete_residue_snapshot(&residue);
    let second_before = delete_residue_snapshot(&second);
    assert!(matches!(
        TransitionJournalStore::open(multiple.path()),
        Err(StorageError::DeleteResidueEntrySetMismatch { .. })
    ));
    assert_eq!(delete_residue_snapshot(&residue), first_before);
    assert_eq!(delete_residue_snapshot(&second), second_before);
}

#[test]
fn delete_residue_recovery_rejects_same_bytes_different_inode_before_restore() {
    let (temporary, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let original = fs::symlink_metadata(&residue).unwrap();
    let bytes = delete_residue_snapshot(&residue).bytes;
    let callback_residue = residue.clone();
    let callback_bytes = bytes.clone();
    arm_delete_residue_recovery_revalidation_callback(
        DeleteResidueRecoveryRevalidationBoundary::BetweenLayoutObservations,
        move || {
            fs::remove_file(&callback_residue).unwrap();
            write_bound_delete_replacement(&callback_residue, &callback_bytes);
        },
    );

    let error = TransitionJournalStore::open(temporary.path()).unwrap_err();

    assert_delete_residue_recovery_revalidation_callback_consumed();
    assert!(matches!(error, StorageError::DeleteResidueChanged { .. }));
    let replacement = fs::symlink_metadata(&residue).unwrap();
    assert_ne!((original.dev(), original.ino()), (replacement.dev(), replacement.ino()));
    assert_eq!(delete_residue_snapshot(&residue).bytes, bytes);
    assert!(!canonical(temporary.path()).exists());
}

#[test]
fn delete_residue_recovery_rejects_same_inode_framed_byte_mutation_between_observations() {
    let (temporary, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let original = fs::symlink_metadata(&residue).unwrap();
    let mut changed = delete_residue_snapshot(&residue).bytes;
    let last = changed.last_mut().unwrap();
    *last ^= 1;
    let callback_residue = residue.clone();
    let callback_changed = changed.clone();
    arm_delete_residue_recovery_revalidation_callback(
        DeleteResidueRecoveryRevalidationBoundary::BetweenLayoutObservations,
        move || fs::write(&callback_residue, &callback_changed).unwrap(),
    );

    let error = TransitionJournalStore::open(temporary.path()).unwrap_err();

    assert_delete_residue_recovery_revalidation_callback_consumed();
    assert!(matches!(error, StorageError::DeleteResidueChanged { .. }));
    let after = fs::symlink_metadata(&residue).unwrap();
    assert_eq!((original.dev(), original.ino()), (after.dev(), after.ino()));
    assert_eq!(delete_residue_snapshot(&residue).bytes, changed);
    assert!(!canonical(temporary.path()).exists());
}

#[test]
fn delete_residue_recovery_rejects_same_bytes_different_inode_after_restore() {
    let (temporary, _terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let original = fs::symlink_metadata(&residue).unwrap();
    let bytes = delete_residue_snapshot(&residue).bytes;
    let canonical_path = canonical(temporary.path());
    let callback_canonical = canonical_path.clone();
    let callback_bytes = bytes.clone();
    arm_delete_residue_recovery_revalidation_callback(
        DeleteResidueRecoveryRevalidationBoundary::BeforeRestoredCanonicalSync,
        move || {
            fs::remove_file(&callback_canonical).unwrap();
            write_bound_delete_replacement(&callback_canonical, &callback_bytes);
        },
    );

    let error = TransitionJournalStore::open(temporary.path()).unwrap_err();

    assert_delete_residue_recovery_revalidation_callback_consumed();
    assert!(matches!(error, StorageError::DeleteResidueChanged { .. }));
    let replacement = fs::symlink_metadata(&canonical_path).unwrap();
    assert_ne!((original.dev(), original.ino()), (replacement.dev(), replacement.ino()));
    assert_eq!(delete_residue_snapshot(&canonical_path).bytes, bytes);
    assert!(!residue.exists());
}

#[derive(Clone, Copy, Debug)]
enum DeleteRecoveryPublicReplacement {
    Journal,
    Lock,
    Inventory,
}

#[test]
fn delete_residue_recovery_revalidates_public_journal_lock_and_inventory_at_every_mutation_seam() {
    let boundaries = [
        DeleteResidueRecoveryRevalidationBoundary::BetweenLayoutObservations,
        DeleteResidueRecoveryRevalidationBoundary::BeforeRestoreFinalBinding,
        DeleteResidueRecoveryRevalidationBoundary::BeforeRestoredCanonicalSync,
        DeleteResidueRecoveryRevalidationBoundary::BeforeFinalCanonicalBinding,
    ];
    let replacements = [
        DeleteRecoveryPublicReplacement::Journal,
        DeleteRecoveryPublicReplacement::Lock,
        DeleteRecoveryPublicReplacement::Inventory,
    ];
    let mut cases = 0;
    for boundary in boundaries {
        for replacement in replacements {
            let (temporary, _terminal, residue) =
                prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
            let root = temporary.path().to_owned();
            let journal = root.join(".cast/journal");
            let parked_journal = root.join(".cast/retained-journal");
            let parked_lock = journal.join("retained-lock");
            let foreign = journal.join("foreign-evidence");
            let callback_parked_journal = parked_journal.clone();
            let callback_parked_lock = parked_lock.clone();
            let callback_foreign = foreign.clone();
            arm_delete_residue_recovery_revalidation_callback(boundary, move || match replacement {
                DeleteRecoveryPublicReplacement::Journal => {
                    fs::rename(&journal, &callback_parked_journal).unwrap();
                    fs::create_dir(&journal).unwrap();
                    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
                    let replacement_lock = journal.join("state-transition.lock");
                    fs::write(&replacement_lock, b"").unwrap();
                    fs::set_permissions(replacement_lock, fs::Permissions::from_mode(0o600)).unwrap();
                }
                DeleteRecoveryPublicReplacement::Lock => {
                    fs::rename(journal.join("state-transition.lock"), &callback_parked_lock).unwrap();
                    let replacement_lock = journal.join("state-transition.lock");
                    fs::write(&replacement_lock, b"").unwrap();
                    fs::set_permissions(replacement_lock, fs::Permissions::from_mode(0o600)).unwrap();
                }
                DeleteRecoveryPublicReplacement::Inventory => {
                    fs::write(&callback_foreign, b"foreign race evidence").unwrap();
                }
            });

            let error = TransitionJournalStore::open(temporary.path()).unwrap_err();

            assert_delete_residue_recovery_revalidation_callback_consumed();
            match replacement {
                DeleteRecoveryPublicReplacement::Journal => {
                    assert!(matches!(error, StorageError::JournalDirectoryBindingChanged));
                    assert_eq!(
                        delete_recovery_journal_names(temporary.path()),
                        [b"state-transition.lock".to_vec()]
                    );
                    let retained_record = if matches!(
                        boundary,
                        DeleteResidueRecoveryRevalidationBoundary::BetweenLayoutObservations
                            | DeleteResidueRecoveryRevalidationBoundary::BeforeRestoreFinalBinding
                    ) {
                        parked_journal.join(RECOVERY_RESIDUE_NAME)
                    } else {
                        parked_journal.join("state-transition")
                    };
                    assert!(retained_record.exists());
                }
                DeleteRecoveryPublicReplacement::Lock => {
                    assert!(matches!(error, StorageError::JournalLockBindingChanged));
                    assert!(parked_lock.exists());
                    let public_record = if matches!(
                        boundary,
                        DeleteResidueRecoveryRevalidationBoundary::BetweenLayoutObservations
                            | DeleteResidueRecoveryRevalidationBoundary::BeforeRestoreFinalBinding
                    ) {
                        residue.clone()
                    } else {
                        canonical(temporary.path())
                    };
                    assert!(public_record.exists());
                }
                DeleteRecoveryPublicReplacement::Inventory => {
                    assert!(matches!(error, StorageError::DeleteResidueEntrySetMismatch { .. }));
                    assert_eq!(fs::read(&foreign).unwrap(), b"foreign race evidence");
                    let public_record = if matches!(
                        boundary,
                        DeleteResidueRecoveryRevalidationBoundary::BetweenLayoutObservations
                            | DeleteResidueRecoveryRevalidationBoundary::BeforeRestoreFinalBinding
                    ) {
                        residue.clone()
                    } else {
                        canonical(temporary.path())
                    };
                    assert!(public_record.exists());
                }
            }
            cases += 1;
        }
    }
    assert_eq!(cases, 12, "delete-residue public seam matrix drifted");
}

#[test]
fn delete_residue_recovery_restore_faults_are_fresh_reopen_idempotent_without_retry() {
    let (before_restore, terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    arm_storage_fault(StorageFaultPoint::DeleteResidueRestore);
    let error = TransitionJournalStore::open(before_restore.path()).unwrap_err();
    assert_storage_fault_consumed();
    assert!(matches!(
        error,
        StorageError::RestoreDeleteResidue { restored: false, .. }
    ));
    assert!(residue.exists());
    assert!(!canonical(before_restore.path()).exists());
    let reopened = TransitionJournalStore::open(before_restore.path()).unwrap();
    assert_eq!(reopened.load().unwrap(), Some(terminal));

    let (after_restore, terminal, residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    arm_storage_fault(StorageFaultPoint::DeleteResidueRestoreReport);
    let error = TransitionJournalStore::open(after_restore.path()).unwrap_err();
    assert_storage_fault_consumed();
    assert!(matches!(
        error,
        StorageError::RestoreDeleteResidue { restored: true, .. }
    ));
    assert!(!residue.exists());
    assert!(canonical(after_restore.path()).exists());
    let reopened = TransitionJournalStore::open(after_restore.path()).unwrap();
    assert_eq!(reopened.load().unwrap(), Some(terminal));
}

#[test]
fn delete_residue_recovery_directory_sync_failure_leaves_exact_canonical_for_reopen() {
    let mut cases = 0;
    for fault in [
        StorageFaultPoint::DeleteResidueDirectorySync,
        StorageFaultPoint::DeleteResidueDirectorySyncReport,
    ] {
        let (temporary, terminal, residue) =
            prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
        let retained = delete_residue_snapshot(&residue);
        arm_storage_fault(fault);

        let error = TransitionJournalStore::open(temporary.path()).unwrap_err();

        assert_storage_fault_consumed();
        assert!(matches!(error, StorageError::SyncJournalDirectory { .. }));
        assert!(!residue.exists());
        assert_same_delete_residue_inode_and_frame(
            &delete_residue_snapshot(&canonical(temporary.path())),
            &retained,
        );
        let reopened = TransitionJournalStore::open(temporary.path()).unwrap();
        assert_eq!(reopened.load().unwrap(), Some(terminal));
        cases += 1;
    }
    assert_eq!(cases, 2, "delete-residue directory-sync fault matrix drifted");
}

#[test]
fn delete_residue_recovery_durability_boundaries_follow_restore_then_directory_sync() {
    let (temporary, terminal, _residue) =
        prepare_terminal_delete_residue(BoundDeleteTerminal::RollbackComplete);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let restored_observed = Arc::clone(&observed);
    arm_delete_residue_recovery_durability_callback(
        DeleteResidueRecoveryDurabilityBoundary::CanonicalRestored,
        move || {
            restored_observed
                .lock()
                .unwrap()
                .push(DeleteResidueRecoveryDurabilityBoundary::CanonicalRestored);
            let synced_observed = Arc::clone(&restored_observed);
            arm_delete_residue_recovery_durability_callback(
                DeleteResidueRecoveryDurabilityBoundary::JournalDirectorySynced,
                move || {
                    synced_observed
                        .lock()
                        .unwrap()
                        .push(DeleteResidueRecoveryDurabilityBoundary::JournalDirectorySynced);
                },
            );
        },
    );

    let reopened = TransitionJournalStore::open(temporary.path()).unwrap();

    assert_delete_residue_recovery_durability_callback_consumed();
    assert_eq!(
        *observed.lock().unwrap(),
        [
            DeleteResidueRecoveryDurabilityBoundary::CanonicalRestored,
            DeleteResidueRecoveryDurabilityBoundary::JournalDirectorySynced,
        ]
    );
    assert_eq!(reopened.load().unwrap(), Some(terminal));
}

#[test]
fn delete_residue_recovery_read_only_inspection_refuses_residue_unchanged() {
    let temporary = crate::test_support::private_installation_tempdir();
    let installation = crate::Installation::open(temporary.path(), None).unwrap();
    let store = TransitionJournalStore::open_retained(installation.root_directory(), temporary.path()).unwrap();
    let initial = creation_record();
    store.create(&initial).unwrap();
    let terminal = advance_to_complete(&store, initial);
    drop(store);
    drop(installation);
    let residue = temporary.path().join(".cast/journal").join(RECOVERY_RESIDUE_NAME);
    fs::rename(canonical(temporary.path()), &residue).unwrap();
    let before = delete_residue_snapshot(&residue);
    let names = delete_recovery_journal_names(temporary.path());
    let installation = crate::Installation::open_read_only(temporary.path(), None).unwrap();

    let error = CleanReadOnlyJournal::inspect(&installation).unwrap_err();

    assert!(matches!(
        error,
        ReadOnlyJournalError::UnexpectedEntry(name) if name == RECOVERY_RESIDUE_NAME
    ));
    assert_eq!(delete_residue_snapshot(&residue), before);
    assert_eq!(delete_recovery_journal_names(temporary.path()), names);
    assert_eq!(terminal.phase, Phase::Complete);
}
