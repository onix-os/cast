fn bound_usr_exchanged_fixture() -> (
    tempfile::TempDir,
    TransitionJournalStore,
    fs::File,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    let (temporary, store) = fixture();
    let mut current = creation_record();
    store.create(&current).unwrap();
    while current.phase != Phase::UsrExchanged {
        let next = legal_forward_advance(&current);
        store.advance(&current, &next).unwrap();
        current = next;
    }
    let cast = fs::File::open(temporary.path().join(".cast")).unwrap();
    let binding = store.record_binding(&cast, &current).unwrap();
    (temporary, store, cast, current, binding)
}

#[test]
fn bound_record_advance_returns_the_exact_public_successor_binding() {
    let (_temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
    assert!(store.has_record_binding(&cast, &binding, &current).unwrap());
    let next = legal_forward_advance(&current);

    let successor = store.advance_record_binding(&cast, binding, &next).unwrap();

    assert_eq!(next.phase, Phase::RootLinksComplete);
    assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), Some(next.clone()));
    assert!(store.has_record_binding(&cast, &successor, &next).unwrap());
    assert!(!store.has_record_binding(&cast, &successor, &current).unwrap());
}

#[test]
fn bound_record_advance_rejects_an_invalid_successor_without_mutation() {
    let (_temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
    let mut invalid = legal_forward_advance(&current);
    invalid.generation += 1;

    let error = store.advance_record_binding(&cast, binding, &invalid).unwrap_err();

    assert!(matches!(error, StorageError::InvalidAdvance(_)));
    assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), Some(current));
}

#[test]
fn bound_record_advance_rejects_a_binding_from_another_open_store() {
    let (_first_temporary, first_store, first_cast, first, first_binding) = bound_usr_exchanged_fixture();
    let (_second_temporary, second_store, second_cast, second, _second_binding) = bound_usr_exchanged_fixture();
    let next = legal_forward_advance(&second);

    let error = second_store
        .advance_record_binding(&second_cast, first_binding, &next)
        .unwrap_err();

    assert!(matches!(error, StorageError::CanonicalChanged));
    assert_eq!(first_store.load_revalidated_retained_cast(&first_cast).unwrap(), Some(first));
    assert_eq!(second_store.load_revalidated_retained_cast(&second_cast).unwrap(), Some(second));
}

#[test]
fn bound_record_advance_rejects_same_bytes_predecessor_replacement_before_publication() {
    let (temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
    let next = legal_forward_advance(&current);
    let canonical_path = canonical(temporary.path());
    let displaced = temporary.path().join("bound-predecessor-displaced");
    let bytes = fs::read(&canonical_path).unwrap();
    let callback_canonical = canonical_path.clone();
    let callback_displaced = displaced.clone();
    let callback_bytes = bytes.clone();
    arm_public_binding_revalidation_callback(PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish, move || {
        fs::rename(&callback_canonical, &callback_displaced).unwrap();
        fs::write(&callback_canonical, callback_bytes).unwrap();
        fs::set_permissions(&callback_canonical, fs::Permissions::from_mode(0o600)).unwrap();
    });

    let error = store.advance_record_binding(&cast, binding, &next).unwrap_err();

    assert_public_binding_revalidation_callback_consumed();
    assert!(matches!(error, StorageError::CanonicalChanged));
    assert_eq!(fs::read(displaced).unwrap(), bytes);
    assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), Some(current));
    assert_no_journal_temporaries(temporary.path());
}

#[test]
fn bound_record_advance_revalidates_public_journal_lock_and_inventory_before_publication() {
    for mutation in ["journal", "lock", "entry"] {
        let (temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
        let next = legal_forward_advance(&current);
        let cast_path = temporary.path().join(".cast");
        let journal = cast_path.join("journal");
        let lock = journal.join("state-transition.lock");
        let displaced = cast_path.join(format!("bound-{mutation}-displaced"));
        let callback_journal = journal.clone();
        let callback_lock = lock.clone();
        let callback_displaced = displaced.clone();
        arm_public_binding_revalidation_callback(
            PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,
            move || match mutation {
                "journal" => {
                    fs::rename(&callback_journal, &callback_displaced).unwrap();
                    fs::create_dir(&callback_journal).unwrap();
                    fs::set_permissions(&callback_journal, fs::Permissions::from_mode(0o700)).unwrap();
                }
                "lock" => {
                    fs::rename(&callback_lock, &callback_displaced).unwrap();
                    fs::write(&callback_lock, b"replacement-lock").unwrap();
                    fs::set_permissions(&callback_lock, fs::Permissions::from_mode(0o600)).unwrap();
                }
                "entry" => fs::write(callback_journal.join("unexpected-entry"), b"unexpected").unwrap(),
                _ => unreachable!(),
            },
        );

        let error = store.advance_record_binding(&cast, binding, &next).unwrap_err();

        assert_public_binding_revalidation_callback_consumed();
        assert!(match mutation {
            "journal" => matches!(error, StorageError::JournalDirectoryBindingChanged),
            "lock" => matches!(error, StorageError::JournalLockBindingChanged),
            "entry" => matches!(error, StorageError::JournalEntrySetMismatch { .. }),
            _ => unreachable!(),
        });
        assert_eq!(store.load().unwrap(), Some(current));
    }
}

#[test]
fn bound_record_advance_rejects_same_bytes_successor_replacement_after_publication() {
    let (temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
    let next = legal_forward_advance(&current);
    let next_bytes = encode(&next).unwrap();
    let canonical_path = canonical(temporary.path());
    let displaced = temporary.path().join("bound-successor-displaced");
    let callback_canonical = canonical_path.clone();
    let callback_displaced = displaced.clone();
    let callback_bytes = next_bytes.clone();
    arm_public_binding_revalidation_callback(
        PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding,
        move || {
            fs::rename(&callback_canonical, &callback_displaced).unwrap();
            fs::write(&callback_canonical, callback_bytes).unwrap();
            fs::set_permissions(&callback_canonical, fs::Permissions::from_mode(0o600)).unwrap();
        },
    );

    let error = store.advance_record_binding(&cast, binding, &next).unwrap_err();

    assert_public_binding_revalidation_callback_consumed();
    assert!(matches!(error, StorageError::CanonicalChanged));
    assert_eq!(fs::read(displaced).unwrap(), next_bytes);
    assert_eq!(store.load_revalidated_retained_cast(&cast).unwrap(), Some(next));
    assert_no_journal_temporaries(temporary.path());
}

#[test]
fn bound_record_advance_faults_expose_only_the_predecessor_or_successor() {
    for (point, successor_visible) in [
        (StorageFaultPoint::TemporarySync, false),
        (StorageFaultPoint::UpdateExchange, false),
        (StorageFaultPoint::UpdateFirstDirectorySync, true),
        (StorageFaultPoint::DisplacedUnlink, true),
        (StorageFaultPoint::UpdateFinalDirectorySync, true),
    ] {
        let (_temporary, store, cast, current, binding) = bound_usr_exchanged_fixture();
        let next = legal_forward_advance(&current);
        arm_storage_fault(point);

        let error = store.advance_record_binding(&cast, binding, &next).unwrap_err();

        assert_storage_fault_consumed();
        assert!(matches!(
            error,
            StorageError::SyncTemporary { .. }
                | StorageError::PublishCanonical { .. }
                | StorageError::SyncJournalDirectory { .. }
                | StorageError::DeleteDisplaced { .. }
        ));
        assert_eq!(store.load().unwrap(), Some(if successor_visible { next } else { current }));
    }
}
