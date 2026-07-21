#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundDeleteSeam {
    Admission,
    Detach,
    PrivateUnlink,
    PostUnlink,
    Publication,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundDeleteTerminal {
    Complete,
    RollbackComplete,
}

impl BoundDeleteTerminal {
    const ALL: [Self; 2] = [Self::Complete, Self::RollbackComplete];
}

impl BoundDeleteSeam {
    const ALL: [Self; 5] = [
        Self::Admission,
        Self::Detach,
        Self::PrivateUnlink,
        Self::PostUnlink,
        Self::Publication,
    ];

    fn boundary(self) -> PublicBindingRevalidationBoundary {
        match self {
            Self::Admission => PublicBindingRevalidationBoundary::BeforeBoundDeleteAdmission,
            Self::Detach => PublicBindingRevalidationBoundary::BeforeBoundDeleteDetach,
            Self::PrivateUnlink => PublicBindingRevalidationBoundary::BeforeBoundDeletePrivateUnlink,
            Self::PostUnlink => PublicBindingRevalidationBoundary::AfterBoundDeleteUnlink,
            Self::Publication => PublicBindingRevalidationBoundary::BeforeBoundDeletePublication,
        }
    }
}

fn bound_delete_fixture() -> (
    tempfile::TempDir,
    TransitionJournalStore,
    fs::File,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    let (temporary, store) = fixture();
    let mut initial = archived_record(Phase::Preparing);
    initial.generation = 1;
    store.create(&initial).unwrap();
    let rollback = satisfied_preparing_rollback(&initial);
    store.advance(&initial, &rollback).unwrap();
    let terminal = advance_record(&rollback, Phase::RollbackComplete);
    store.advance(&rollback, &terminal).unwrap();
    let cast = fs::File::open(temporary.path().join(".cast")).unwrap();
    let binding = store.record_binding(&cast, &terminal).unwrap();
    (temporary, store, cast, terminal, binding)
}

fn bound_delete_terminal_fixture(
    terminal_phase: BoundDeleteTerminal,
) -> (
    tempfile::TempDir,
    TransitionJournalStore,
    fs::File,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    match terminal_phase {
        BoundDeleteTerminal::RollbackComplete => bound_delete_fixture(),
        BoundDeleteTerminal::Complete => {
            let (temporary, store) = fixture();
            let initial = creation_record();
            store.create(&initial).unwrap();
            let terminal = advance_to_complete(&store, initial);
            let cast = fs::File::open(temporary.path().join(".cast")).unwrap();
            let binding = store.record_binding(&cast, &terminal).unwrap();
            (temporary, store, cast, terminal, binding)
        }
    }
}

fn bound_delete_private_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(root.join(".cast/journal"))
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            entry
                .file_name()
                .as_bytes()
                .starts_with(DELETE_PREFIX)
                .then(|| entry.path())
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn bound_delete_private_path(root: &Path) -> PathBuf {
    let paths = bound_delete_private_paths(root);
    assert_eq!(paths.len(), 1, "expected one bound-delete private record: {paths:?}");
    paths.into_iter().next().unwrap()
}

fn write_bound_delete_replacement(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn bound_delete_checkpoints(seam: BoundDeleteSeam) -> Vec<DurabilityCheckpoint> {
    match seam {
        BoundDeleteSeam::Admission | BoundDeleteSeam::Detach => Vec::new(),
        BoundDeleteSeam::PrivateUnlink => vec![DurabilityCheckpoint::CanonicalDetached],
        BoundDeleteSeam::PostUnlink => vec![
            DurabilityCheckpoint::CanonicalDetached,
            DurabilityCheckpoint::CanonicalUnlinked,
        ],
        BoundDeleteSeam::Publication => vec![
            DurabilityCheckpoint::CanonicalDetached,
            DurabilityCheckpoint::CanonicalUnlinked,
            DurabilityCheckpoint::JournalDirectorySynced,
        ],
    }
}

fn assert_bound_delete_public_error(
    error: TransitionJournalRecordDeleteError,
    seam: BoundDeleteSeam,
    expected: &str,
) {
    let source = match (seam, error) {
        (BoundDeleteSeam::Admission, TransitionJournalRecordDeleteError::Admission(source)) => source,
        (
            BoundDeleteSeam::Detach | BoundDeleteSeam::PrivateUnlink,
            TransitionJournalRecordDeleteError::Detached(source),
        ) => source,
        (
            BoundDeleteSeam::PostUnlink | BoundDeleteSeam::Publication,
            TransitionJournalRecordDeleteError::PostDelete(source),
        ) => source,
        (_, other) => panic!("unexpected {seam:?} bound-delete error: {other:?}"),
    };
    assert!(
        match expected {
            "canonical" => matches!(source, StorageError::CanonicalChanged),
            "journal" => matches!(source, StorageError::JournalDirectoryBindingChanged),
            "lock" => matches!(source, StorageError::JournalLockBindingChanged),
            _ => unreachable!(),
        },
        "unexpected {seam:?} {expected} error: {source:?}"
    );
}

#[test]
fn bound_record_delete_consumes_exact_terminal_binding_and_returns_clean_locked_store() {
    let mut cases = 0;
    for terminal_phase in BoundDeleteTerminal::ALL {
        let (temporary, store, cast, terminal, binding) = bound_delete_terminal_fixture(terminal_phase);
        take_durability_checkpoints();

        store.delete_record_binding(&cast, binding, &terminal).unwrap();

        assert_eq!(
            take_durability_checkpoints(),
            [
                DurabilityCheckpoint::CanonicalDetached,
                DurabilityCheckpoint::CanonicalUnlinked,
                DurabilityCheckpoint::JournalDirectorySynced,
            ]
        );
        assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), None);
        store.revalidate_retained_cast_binding(&cast).unwrap();
        let names = fs::read_dir(temporary.path().join(".cast/journal"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, ["state-transition.lock"]);
        assert!(bound_delete_private_paths(temporary.path()).is_empty());
        assert!(!store.delete(&terminal).unwrap());
        cases += 1;
    }
    assert_eq!(cases, 2, "bound-delete terminal phase matrix drifted");
}

#[test]
fn bound_record_delete_rejects_wrong_store_record_phase_and_cast_without_unlink() {
    let (_first_temporary, first_store, first_cast, first_terminal, first_binding) = bound_delete_fixture();
    let (second_temporary, second_store, second_cast, second_terminal, _second_binding) = bound_delete_fixture();
    let error = second_store
        .delete_record_binding(&second_cast, first_binding, &second_terminal)
        .unwrap_err();
    assert!(matches!(
        error,
        TransitionJournalRecordDeleteError::Admission(StorageError::CanonicalChanged)
    ));
    assert_eq!(first_store.load_revalidated_retained_cast(&first_cast).unwrap(), Some(first_terminal));
    assert_eq!(
        second_store.load_revalidated_retained_cast(&second_cast).unwrap(),
        Some(second_terminal)
    );

    let (_temporary, store, cast, terminal, binding) = bound_delete_fixture();
    let mut different = terminal.clone();
    different.quarantine_name = QuarantineName::parse("failed-fedcba9876543210").unwrap();
    let error = store.delete_record_binding(&cast, binding, &different).unwrap_err();
    assert!(matches!(
        error,
        TransitionJournalRecordDeleteError::Admission(StorageError::ExpectedRecordMismatch)
    ));
    assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), Some(terminal));

    let (temporary, store) = fixture();
    let preparing = creation_record();
    store.create(&preparing).unwrap();
    let cast = fs::File::open(temporary.path().join(".cast")).unwrap();
    let binding = store.record_binding(&cast, &preparing).unwrap();
    let error = store.delete_record_binding(&cast, binding, &preparing).unwrap_err();
    assert!(matches!(
        error,
        TransitionJournalRecordDeleteError::Admission(StorageError::DeleteNonterminal)
    ));
    assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), Some(preparing));

    let (_temporary, store, _cast, terminal, binding) = bound_delete_fixture();
    let wrong_cast = fs::File::open(second_temporary.path().join(".cast")).unwrap();
    let error = store
        .delete_record_binding(&wrong_cast, binding, &terminal)
        .unwrap_err();
    assert!(matches!(
        error,
        TransitionJournalRecordDeleteError::Admission(StorageError::JournalDirectoryBindingChanged)
    ));
    assert_eq!(store.load().unwrap(), Some(terminal));
}

#[test]
fn bound_record_delete_same_byte_inode_replacement_at_every_seam_never_deletes_replacement() {
    let mut cases = 0;
    for seam in BoundDeleteSeam::ALL {
        let (temporary, store, cast, terminal, binding) = bound_delete_fixture();
        let canonical_path = canonical(temporary.path());
        let journal = temporary.path().join(".cast/journal");
        let displaced = temporary.path().join(format!("bound-delete-{seam:?}-displaced"));
        let bytes = fs::read(&canonical_path).unwrap();
        let original = fs::symlink_metadata(&canonical_path).unwrap();
        let callback_root = temporary.path().to_path_buf();
        let callback_canonical = canonical_path.clone();
        let callback_journal = journal.clone();
        let callback_displaced = displaced.clone();
        let callback_bytes = bytes.clone();
        arm_public_binding_revalidation_callback(seam.boundary(), move || match seam {
            BoundDeleteSeam::Admission | BoundDeleteSeam::Detach => {
                fs::rename(&callback_canonical, &callback_displaced).unwrap();
                write_bound_delete_replacement(&callback_canonical, &callback_bytes);
            }
            BoundDeleteSeam::PrivateUnlink => {
                let private = bound_delete_private_path(&callback_root);
                fs::rename(&private, &callback_displaced).unwrap();
                write_bound_delete_replacement(&private, &callback_bytes);
            }
            BoundDeleteSeam::PostUnlink | BoundDeleteSeam::Publication => {
                write_bound_delete_replacement(&callback_journal.join("state-transition"), &callback_bytes);
            }
        });
        take_durability_checkpoints();

        let error = store.delete_record_binding(&cast, binding, &terminal).unwrap_err();

        assert_public_binding_revalidation_callback_consumed();
        assert_bound_delete_public_error(error, seam, "canonical");
        assert_eq!(take_durability_checkpoints(), bound_delete_checkpoints(seam));
        let replacement_path = match seam {
            BoundDeleteSeam::Detach | BoundDeleteSeam::PrivateUnlink => {
                bound_delete_private_path(temporary.path())
            }
            _ => canonical_path.clone(),
        };
        assert_eq!(fs::read(&replacement_path).unwrap(), bytes);
        let replacement = fs::symlink_metadata(&replacement_path).unwrap();
        assert_ne!((original.dev(), original.ino()), (replacement.dev(), replacement.ino()));
        assert_eq!(
            displaced.exists(),
            matches!(seam, BoundDeleteSeam::Admission | BoundDeleteSeam::Detach | BoundDeleteSeam::PrivateUnlink)
        );
        if displaced.exists() {
            assert_eq!(fs::read(&displaced).unwrap(), bytes);
        }
        cases += 1;
    }
    assert_eq!(cases, 5, "bound-delete canonical seam matrix drifted");
}

#[test]
fn bound_record_delete_public_journal_and_lock_replacement_at_every_seam_fail_closed() {
    let mut cases = 0;
    for seam in BoundDeleteSeam::ALL {
        for mutation in ["journal", "lock"] {
            let (temporary, store, cast, terminal, binding) = bound_delete_fixture();
            let cast_path = temporary.path().join(".cast");
            let journal = cast_path.join("journal");
            let lock = journal.join("state-transition.lock");
            let displaced = cast_path.join(format!("bound-delete-{seam:?}-{mutation}-displaced"));
            let bytes = fs::read(canonical(temporary.path())).unwrap();
            let callback_journal = journal.clone();
            let callback_lock = lock.clone();
            let callback_displaced = displaced.clone();
            let callback_bytes = bytes.clone();
            arm_public_binding_revalidation_callback(seam.boundary(), move || match mutation {
                "journal" => {
                    fs::rename(&callback_journal, &callback_displaced).unwrap();
                    fs::create_dir(&callback_journal).unwrap();
                    fs::set_permissions(&callback_journal, fs::Permissions::from_mode(0o700)).unwrap();
                    write_bound_delete_replacement(
                        &callback_journal.join("state-transition.lock"),
                        b"replacement-lock",
                    );
                    write_bound_delete_replacement(
                        &callback_journal.join("state-transition"),
                        &callback_bytes,
                    );
                }
                "lock" => {
                    fs::rename(&callback_lock, &callback_displaced).unwrap();
                    write_bound_delete_replacement(&callback_lock, b"replacement-lock");
                }
                _ => unreachable!(),
            });
            take_durability_checkpoints();

            let error = store.delete_record_binding(&cast, binding, &terminal).unwrap_err();

            assert_public_binding_revalidation_callback_consumed();
            assert_bound_delete_public_error(error, seam, mutation);
            assert_eq!(take_durability_checkpoints(), bound_delete_checkpoints(seam));
            match mutation {
                "journal" => {
                    assert_eq!(fs::read(journal.join("state-transition")).unwrap(), bytes);
                    assert_eq!(fs::read(journal.join("state-transition.lock")).unwrap(), b"replacement-lock");
                }
                "lock" => assert_eq!(fs::read(&lock).unwrap(), b"replacement-lock"),
                _ => unreachable!(),
            }
            assert!(displaced.exists());
            cases += 1;
        }
    }
    assert_eq!(cases, 10, "bound-delete public identity seam matrix drifted");
}

#[test]
fn bound_record_delete_final_publication_sandwich_rejects_observation_gap_replacements() {
    let mut cases = 0;
    for mutation in ["journal", "lock"] {
        let (temporary, store, cast, terminal, binding) = bound_delete_fixture();
        let cast_path = temporary.path().join(".cast");
        let journal = cast_path.join("journal");
        let lock = journal.join("state-transition.lock");
        let displaced = cast_path.join(format!("bound-delete-final-binding-{mutation}-displaced"));
        let bytes = encode(&terminal).unwrap();
        let callback_journal = journal.clone();
        let callback_lock = lock.clone();
        let callback_displaced = displaced.clone();
        let callback_bytes = bytes.clone();
        arm_public_binding_revalidation_callback(
            PublicBindingRevalidationBoundary::BeforeBoundDeletePublicationFinalBinding,
            move || match mutation {
                "journal" => {
                    fs::rename(&callback_journal, &callback_displaced).unwrap();
                    fs::create_dir(&callback_journal).unwrap();
                    fs::set_permissions(&callback_journal, fs::Permissions::from_mode(0o700)).unwrap();
                    write_bound_delete_replacement(
                        &callback_journal.join("state-transition.lock"),
                        b"replacement-lock",
                    );
                    write_bound_delete_replacement(
                        &callback_journal.join("state-transition"),
                        &callback_bytes,
                    );
                }
                "lock" => {
                    fs::rename(&callback_lock, &callback_displaced).unwrap();
                    write_bound_delete_replacement(&callback_lock, b"replacement-lock");
                }
                _ => unreachable!(),
            },
        );
        take_durability_checkpoints();

        let error = store.delete_record_binding(&cast, binding, &terminal).unwrap_err();

        assert_public_binding_revalidation_callback_consumed();
        let source = match error {
            TransitionJournalRecordDeleteError::PostDelete(source) => source,
            other => panic!("unexpected final-binding {mutation} error: {other:?}"),
        };
        assert!(match mutation {
            "journal" => matches!(source, StorageError::JournalDirectoryBindingChanged),
            "lock" => matches!(source, StorageError::JournalLockBindingChanged),
            _ => unreachable!(),
        });
        assert_eq!(
            take_durability_checkpoints(),
            [
                DurabilityCheckpoint::CanonicalDetached,
                DurabilityCheckpoint::CanonicalUnlinked,
                DurabilityCheckpoint::JournalDirectorySynced,
            ]
        );
        match mutation {
            "journal" => assert_eq!(fs::read(journal.join("state-transition")).unwrap(), bytes),
            "lock" => assert_eq!(fs::read(&lock).unwrap(), b"replacement-lock"),
            _ => unreachable!(),
        }
        cases += 1;
    }
    assert_eq!(cases, 2, "bound-delete final public sandwich matrix drifted");
}

#[test]
fn bound_record_delete_noreplace_collision_preserves_exact_source_and_foreign_winner() {
    let (temporary, store, cast, terminal, binding) = bound_delete_fixture();
    let journal = temporary.path().join(".cast/journal");
    let collision_path = Arc::new(Mutex::new(None));
    let callback_collision = Arc::clone(&collision_path);
    arm_bound_delete_private_name_callback(move |name| {
        let path = journal.join(std::ffi::OsStr::from_bytes(name.as_bytes()));
        write_bound_delete_replacement(&path, b"foreign-private-winner");
        *callback_collision.lock().unwrap() = Some(path);
    });
    take_durability_checkpoints();

    let error = store.delete_record_binding(&cast, binding, &terminal).unwrap_err();

    assert_bound_delete_private_name_callback_consumed();
    assert!(matches!(
        error,
        TransitionJournalRecordDeleteError::StorageAndReconciliation {
            storage: StorageError::DetachCanonical { .. },
            reconciliation: StorageError::CanonicalChanged,
        }
    ));
    assert!(take_durability_checkpoints().is_empty());
    assert!(matches!(
        store.load_revalidated_retained_cast(&cast).unwrap_err(),
        StorageError::JournalEntrySetMismatch { .. }
    ));
    assert_eq!(fs::read(canonical(temporary.path())).unwrap(), encode(&terminal).unwrap());
    let collision = collision_path.lock().unwrap().clone().unwrap();
    assert_eq!(fs::read(collision).unwrap(), b"foreign-private-winner");
}

#[test]
fn bound_record_delete_storage_faults_reconcile_exact_source_or_absence_without_retry() {
    let mut cases = 0;
    for (point, expected_state, canonical_present, expected_checkpoints) in [
        (
            StorageFaultPoint::BoundDeleteDetach,
            TransitionJournalRecordDeleteState::ExactSource,
            true,
            vec![],
        ),
        (
            StorageFaultPoint::BoundDeleteDetachReport,
            TransitionJournalRecordDeleteState::Absent,
            false,
            vec![
                DurabilityCheckpoint::CanonicalDetached,
                DurabilityCheckpoint::CanonicalUnlinked,
                DurabilityCheckpoint::JournalDirectorySynced,
            ],
        ),
        (
            StorageFaultPoint::CanonicalUnlink,
            TransitionJournalRecordDeleteState::ExactSource,
            true,
            vec![
                DurabilityCheckpoint::CanonicalDetached,
                DurabilityCheckpoint::JournalDirectorySynced,
            ],
        ),
        (
            StorageFaultPoint::BoundDeleteUnlinkReport,
            TransitionJournalRecordDeleteState::Absent,
            false,
            vec![
                DurabilityCheckpoint::CanonicalDetached,
                DurabilityCheckpoint::CanonicalUnlinked,
                DurabilityCheckpoint::JournalDirectorySynced,
            ],
        ),
        (
            StorageFaultPoint::DeleteDirectorySync,
            TransitionJournalRecordDeleteState::Absent,
            false,
            vec![
                DurabilityCheckpoint::CanonicalDetached,
                DurabilityCheckpoint::CanonicalUnlinked,
            ],
        ),
    ] {
        let (temporary, store, cast, terminal, binding) = bound_delete_fixture();
        arm_storage_fault(point);
        take_durability_checkpoints();

        let error = store.delete_record_binding(&cast, binding, &terminal).unwrap_err();

        assert_storage_fault_consumed();
        match error {
            TransitionJournalRecordDeleteError::Storage { state, source } => {
                assert_eq!(state, expected_state);
                assert!(match point {
                    StorageFaultPoint::BoundDeleteDetach | StorageFaultPoint::BoundDeleteDetachReport => {
                        matches!(source, StorageError::DetachCanonical { .. })
                    }
                    StorageFaultPoint::CanonicalUnlink | StorageFaultPoint::BoundDeleteUnlinkReport => {
                        matches!(source, StorageError::DeleteDetachedCanonical { .. })
                    }
                    StorageFaultPoint::DeleteDirectorySync => {
                        matches!(source, StorageError::SyncJournalDirectory { .. })
                    }
                    _ => unreachable!(),
                });
            }
            other => panic!("unexpected {point:?} bound-delete error: {other:?}"),
        }
        assert_eq!(
            store.load_revalidated_retained_cast(&cast).unwrap(),
            canonical_present.then_some(terminal)
        );
        assert_eq!(take_durability_checkpoints(), expected_checkpoints);
        assert!(bound_delete_private_paths(temporary.path()).is_empty());
        cases += 1;
    }
    assert_eq!(cases, 5, "bound-delete storage reconciliation matrix drifted");
}

#[test]
fn bound_record_delete_storage_reconciliation_never_deletes_same_byte_replacement() {
    let mut cases = 0;
    for point in [
        StorageFaultPoint::BoundDeleteDetach,
        StorageFaultPoint::BoundDeleteDetachReport,
        StorageFaultPoint::CanonicalUnlink,
        StorageFaultPoint::BoundDeleteUnlinkReport,
        StorageFaultPoint::DeleteDirectorySync,
    ] {
        let (temporary, store, cast, terminal, binding) = bound_delete_fixture();
        let canonical_path = canonical(temporary.path());
        let displaced = temporary.path().join(format!("bound-delete-{point:?}-reconciliation-displaced"));
        let bytes = fs::read(&canonical_path).unwrap();
        let original = fs::symlink_metadata(&canonical_path).unwrap();
        let callback_root = temporary.path().to_path_buf();
        let callback_canonical = canonical_path.clone();
        let callback_displaced = displaced.clone();
        let callback_bytes = bytes.clone();
        arm_storage_fault(point);
        arm_public_binding_revalidation_callback(
            PublicBindingRevalidationBoundary::BeforeBoundDeleteFailureReconciliation,
            move || match point {
                StorageFaultPoint::BoundDeleteDetach => {
                    fs::rename(&callback_canonical, &callback_displaced).unwrap();
                    write_bound_delete_replacement(&callback_canonical, &callback_bytes);
                }
                StorageFaultPoint::BoundDeleteDetachReport | StorageFaultPoint::CanonicalUnlink => {
                    let private = bound_delete_private_path(&callback_root);
                    fs::rename(&private, &callback_displaced).unwrap();
                    write_bound_delete_replacement(&private, &callback_bytes);
                }
                StorageFaultPoint::BoundDeleteUnlinkReport | StorageFaultPoint::DeleteDirectorySync => {
                    write_bound_delete_replacement(&callback_canonical, &callback_bytes);
                }
                _ => unreachable!(),
            },
        );
        take_durability_checkpoints();

        let error = store.delete_record_binding(&cast, binding, &terminal).unwrap_err();

        assert_storage_fault_consumed();
        assert_public_binding_revalidation_callback_consumed();
        assert!(matches!(
            error,
            TransitionJournalRecordDeleteError::StorageAndReconciliation { .. }
        ));
        let replacement_path = match point {
            StorageFaultPoint::BoundDeleteDetachReport | StorageFaultPoint::CanonicalUnlink => {
                bound_delete_private_path(temporary.path())
            }
            _ => canonical_path.clone(),
        };
        assert_eq!(fs::read(&replacement_path).unwrap(), bytes);
        let replacement = fs::symlink_metadata(&replacement_path).unwrap();
        assert_ne!((original.dev(), original.ino()), (replacement.dev(), replacement.ino()));
        assert_eq!(
            displaced.exists(),
            matches!(
                point,
                StorageFaultPoint::BoundDeleteDetach
                    | StorageFaultPoint::BoundDeleteDetachReport
                    | StorageFaultPoint::CanonicalUnlink
            )
        );
        assert_eq!(
            take_durability_checkpoints(),
            match point {
                StorageFaultPoint::BoundDeleteDetach | StorageFaultPoint::BoundDeleteDetachReport => vec![],
                StorageFaultPoint::CanonicalUnlink => vec![DurabilityCheckpoint::CanonicalDetached],
                StorageFaultPoint::BoundDeleteUnlinkReport | StorageFaultPoint::DeleteDirectorySync => vec![
                    DurabilityCheckpoint::CanonicalDetached,
                    DurabilityCheckpoint::CanonicalUnlinked,
                ],
                _ => unreachable!(),
            }
        );
        cases += 1;
    }
    assert_eq!(cases, 5, "bound-delete ambiguous storage replacement matrix drifted");
}

#[test]
fn bound_record_delete_rejects_same_inode_record_change_at_both_preunlink_checks() {
    let mut cases = 0;
    for seam in [BoundDeleteSeam::Detach, BoundDeleteSeam::PrivateUnlink] {
        let (temporary, store, cast, terminal, binding) = bound_delete_fixture();
        let canonical_path = canonical(temporary.path());
        let original = fs::symlink_metadata(&canonical_path).unwrap();
        let mut changed = terminal.clone();
        changed.quarantine_name = QuarantineName::parse("failed-fedcba9876543210").unwrap();
        changed.validate().unwrap();
        let changed_bytes = encode(&changed).unwrap();
        let callback_root = temporary.path().to_path_buf();
        let callback_bytes = changed_bytes.clone();
        arm_public_binding_revalidation_callback(seam.boundary(), move || {
            let path = match seam {
                BoundDeleteSeam::Detach => canonical(&callback_root),
                BoundDeleteSeam::PrivateUnlink => bound_delete_private_path(&callback_root),
                _ => unreachable!(),
            };
            fs::write(path, callback_bytes).unwrap();
        });
        take_durability_checkpoints();

        let error = store.delete_record_binding(&cast, binding, &terminal).unwrap_err();

        assert_public_binding_revalidation_callback_consumed();
        assert!(matches!(
            error,
            TransitionJournalRecordDeleteError::Detached(StorageError::CanonicalChanged)
        ));
        assert_eq!(take_durability_checkpoints(), bound_delete_checkpoints(seam));
        let private = bound_delete_private_path(temporary.path());
        let after = fs::symlink_metadata(&private).unwrap();
        assert_eq!((original.dev(), original.ino()), (after.dev(), after.ino()));
        assert_eq!(fs::read(private).unwrap(), changed_bytes);
        cases += 1;
    }
    assert_eq!(cases, 2, "bound-delete same-inode pre-unlink matrix drifted");
}

#[test]
fn bound_record_delete_durability_callbacks_follow_sole_private_unlink_then_sync() {
    let mut cases = 0;
    for report_fault in [false, true] {
        let (_temporary, store, cast, terminal, binding) = bound_delete_fixture();
        if report_fault {
            arm_storage_fault(StorageFaultPoint::BoundDeleteUnlinkReport);
        }
        let observed = Arc::new(Mutex::new(Vec::new()));
        let unlink_observed = Arc::clone(&observed);
        arm_journal_delete_durability_callback(
            JournalDeleteDurabilityBoundary::CanonicalUnlinked,
            move || {
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
            },
        );
        take_durability_checkpoints();

        let result = store.delete_record_binding(&cast, binding, &terminal);

        if report_fault {
            assert_storage_fault_consumed();
            assert!(matches!(
                result,
                Err(TransitionJournalRecordDeleteError::Storage {
                    state: TransitionJournalRecordDeleteState::Absent,
                    source: StorageError::DeleteDetachedCanonical { .. },
                })
            ));
        } else {
            result.unwrap();
        }
        assert_eq!(
            *observed.lock().unwrap(),
            [
                JournalDeleteDurabilityBoundary::CanonicalUnlinked,
                JournalDeleteDurabilityBoundary::DeleteDirectorySynced,
            ]
        );
        assert_eq!(
            take_durability_checkpoints(),
            [
                DurabilityCheckpoint::CanonicalDetached,
                DurabilityCheckpoint::CanonicalUnlinked,
                DurabilityCheckpoint::JournalDirectorySynced,
            ]
        );
        assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), None);
        cases += 1;
    }
    assert_eq!(cases, 2, "bound-delete durability callback matrix drifted");
}
