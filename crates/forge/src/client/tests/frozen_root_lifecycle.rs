#[test]
fn frozen_blit_returns_an_opath_guard_accepted_by_anchored_container() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&frozen_root).unwrap();
    let client = Client::frozen(
        "frozen-activation-anchor-test",
        frozen_test_installation(&installation_root),
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    let package = package::Id::from("directory-only-activation-provider");
    client
        .layout_db
        .add(
            &package,
            &StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFDIR | 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Directory("share".into()),
            },
        )
        .unwrap();
    client.discard_frozen_root().unwrap();

    let materialized = client
        .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
        .unwrap();
    let guard = client
        .require_materialized_frozen_executables(materialized, std::slice::from_ref(&package), &[])
        .unwrap();
    let retained_identity = frozen_root_identity(&guard.root, guard.root_path()).unwrap();
    let anchor = guard.revalidated_anchor().unwrap();
    // SAFETY: the guard retains the borrowed descriptor for this call.
    let status_flags = unsafe { nix::libc::fcntl(anchor.as_raw_fd(), nix::libc::F_GETFL) };
    assert_ne!(status_flags, -1);
    assert_eq!(
        status_flags & (nix::libc::O_PATH | nix::libc::O_DIRECTORY),
        nix::libc::O_PATH | nix::libc::O_DIRECTORY
    );
    // SAFETY: the guard retains the borrowed descriptor for this call.
    let descriptor_flags = unsafe { nix::libc::fcntl(anchor.as_raw_fd(), nix::libc::F_GETFD) };
    assert_ne!(descriptor_flags, -1);
    assert_ne!(descriptor_flags & nix::libc::FD_CLOEXEC, 0);
    let root_locator = container::AnchoredLocator::exact(guard.root_path(), &anchor).unwrap();
    let _container = container::Container::new_anchored(root_locator).unwrap();

    let displaced = temporary.path().join("displaced-frozen-root");
    fs::rename(&frozen_root, &displaced).unwrap();
    fs::create_dir(&frozen_root).unwrap();
    assert_eq!(
        frozen_root_identity(&guard.root, &displaced).unwrap(),
        retained_identity,
        "the guard must retain the pre-publication inode rather than reopen the public path"
    );
    assert_ne!(
        frozen_root_identity(&open_frozen_root_anchor(&frozen_root).unwrap(), &frozen_root).unwrap(),
        retained_identity
    );
    assert!(matches!(
        guard.revalidate(),
        Err(Error::FrozenExecutableRootReplaced(path)) if path == frozen_root
    ));
}

#[test]
fn frozen_publication_rejects_a_readable_activation_descriptor_before_rename() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
    drop(root_anchor);

    let error = publish_frozen_root(&stage, &destination, &root, root.try_clone().unwrap(), deadline).unwrap_err();
    assert!(matches!(
        error,
        Error::FrozenPublicationNamespaceMismatch {
            reason: "the retained activation anchor is not the exact close-on-exec staged O_PATH directory",
            ..
        }
    ));
    assert!(!destination.root_path.exists());
    assert_eq!(
        fs::read(stage.path.join("root/candidate")).unwrap(),
        b"retained candidate"
    );
}

#[test]
fn frozen_publication_rejects_a_foreign_opath_activation_anchor_before_rename() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
    drop(root_anchor);
    let foreign = temporary.path().join("foreign-anchor");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("untouched"), b"foreign inode").unwrap();
    let foreign_anchor = open_frozen_root_anchor(&foreign).unwrap();

    let error = publish_frozen_root(&stage, &destination, &root, foreign_anchor, deadline).unwrap_err();
    assert!(matches!(
        error,
        Error::FrozenPublicationNamespaceMismatch {
            reason: "the retained activation anchor is not the exact close-on-exec staged O_PATH directory",
            ..
        }
    ));
    assert!(!destination.root_path.exists());
    assert_eq!(
        fs::read(stage.path.join("root/candidate")).unwrap(),
        b"retained candidate"
    );
    assert_eq!(fs::read(foreign.join("untouched")).unwrap(), b"foreign inode");
}

#[test]
fn frozen_publication_rejects_an_inheritable_opath_activation_anchor_before_rename() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
    // SAFETY: root_anchor owns a live descriptor for both fcntl calls.
    let descriptor_flags = unsafe { nix::libc::fcntl(root_anchor.as_raw_fd(), nix::libc::F_GETFD) };
    assert_ne!(descriptor_flags, -1);
    // SAFETY: F_SETFD updates only descriptor-local inheritance flags.
    assert_ne!(
        unsafe {
            nix::libc::fcntl(
                root_anchor.as_raw_fd(),
                nix::libc::F_SETFD,
                descriptor_flags & !nix::libc::FD_CLOEXEC,
            )
        },
        -1
    );

    let error = publish_frozen_root(&stage, &destination, &root, root_anchor, deadline).unwrap_err();
    assert!(matches!(
        error,
        Error::FrozenPublicationNamespaceMismatch {
            reason: "the retained activation anchor is not the exact close-on-exec staged O_PATH directory",
            ..
        }
    ));
    assert!(!destination.root_path.exists());
    assert_eq!(
        fs::read(stage.path.join("root/candidate")).unwrap(),
        b"retained candidate"
    );
}

#[test]
fn frozen_root_publication_never_replaces_an_existing_destination() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&frozen_root).unwrap();
    let marker = frozen_root.join("untouched");
    fs::write(&marker, b"original root").unwrap();
    let client = Client::frozen(
        "frozen-existing-destination-test",
        frozen_test_installation(&installation_root),
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    let package = package::Id::from("valid-directory-package");
    client
        .layout_db
        .add(
            &package,
            &StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFDIR | 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Directory("share".into()),
            },
        )
        .unwrap();

    assert!(matches!(
        client.blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000),
        Err(Error::FrozenRootDestinationExists(path)) if path == frozen_root
    ));
    assert_eq!(fs::read(marker).unwrap(), b"original root");

    // Exercise the publication syscall itself, not only the early
    // destination preflight: a destination appearing in that interval is
    // never exchanged or overwritten.
    let deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT;
    let raced_destination = temporary.path().join("raced-destination");
    let raced_parent = open_frozen_destination_parent(temporary.path()).unwrap();
    let raced_destination_authority = FrozenRootDestination {
        root_path: raced_destination.clone(),
        parent_path: temporary.path().to_owned(),
        name: CString::new("raced-destination").unwrap(),
        parent_identity: frozen_root_identity(&raced_parent, temporary.path()).unwrap(),
        parent: raced_parent,
    };
    let _lock = lock_frozen_destination_until(&raced_destination_authority, deadline).unwrap();
    let raced_stage =
        create_frozen_private_directory(&raced_destination_authority, b".publication-test-", deadline).unwrap();
    mkdirat(raced_stage.file.as_raw_fd(), "root", Mode::from_bits_truncate(0o755)).unwrap();
    fs::write(raced_stage.path.join("root/candidate"), b"candidate").unwrap();
    fs::create_dir(&raced_destination).unwrap();
    fs::write(raced_destination.join("winner"), b"winner").unwrap();
    let raced_activation_anchor = openat2_frozen_until(
        raced_stage.file.as_raw_fd(),
        Path::new("root"),
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .unwrap();
    let raced_anchor = openat2_frozen_until(
        raced_stage.file.as_raw_fd(),
        Path::new("root"),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .unwrap();
    assert!(matches!(
        publish_frozen_root(
            &raced_stage,
            &raced_destination_authority,
            &raced_anchor,
            raced_activation_anchor,
            deadline,
        ),
        Err(Error::FrozenRootDestinationExists(path)) if path == raced_destination
    ));
    assert_eq!(fs::read(raced_stage.path.join("root/candidate")).unwrap(), b"candidate");
    assert_eq!(fs::read(raced_destination.join("winner")).unwrap(), b"winner");
}

#[test]
fn frozen_publication_adopts_an_applied_rename_even_when_the_syscall_reports_error() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();

    let materialized = publish_frozen_root_with(
        &stage,
        &destination,
        &root,
        root_anchor,
        deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            renameat2_noreplace_until(
                source_directory.file(),
                source_name,
                destination_directory.file(),
                destination_name,
                deadline,
            )?;
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        },
    )
    .unwrap();

    materialized.revalidate().unwrap();
    assert_eq!(
        fs::read(destination.root_path.join("candidate")).unwrap(),
        b"retained candidate"
    );
    assert!(!stage.path.join("root").exists());
    remove_empty_frozen_private_directory(&stage, &destination, deadline).unwrap();
}

#[test]
fn frozen_publication_reconciles_an_applied_rename_after_the_work_deadline_expires() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, stage, root, root_anchor, fixture_deadline) = frozen_publication_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, fixture_deadline).unwrap();
    let work_deadline = Instant::now() + Duration::from_secs(1);

    let materialized = publish_frozen_root_with(
        &stage,
        &destination,
        &root,
        root_anchor,
        work_deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            renameat2_noreplace_until(
                source_directory.file(),
                source_name,
                destination_directory.file(),
                destination_name,
                work_deadline,
            )?;
            std::thread::sleep(
                work_deadline
                    .saturating_duration_since(Instant::now())
                    .saturating_add(Duration::from_millis(1)),
            );
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        },
    )
    .unwrap();

    materialized.revalidate().unwrap();
    assert_eq!(
        fs::read(destination.root_path.join("candidate")).unwrap(),
        b"retained candidate"
    );
    remove_empty_frozen_private_directory(&stage, &destination, frozen_namespace_recovery_deadline()).unwrap();
}

#[test]
fn frozen_private_directory_setup_failures_remove_the_exact_provisional_wrapper() {
    for rejected in [
        FrozenPrivateDirectoryCheckpoint::Retained,
        FrozenPrivateDirectoryCheckpoint::ModeNormalized,
        FrozenPrivateDirectoryCheckpoint::ReadableOpened,
        FrozenPrivateDirectoryCheckpoint::AclsChecked,
        FrozenPrivateDirectoryCheckpoint::InventoryVerified,
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let destination = frozen_publication_destination(temporary.path(), "published");
        let deadline = Instant::now() + Duration::from_secs(10);
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        let reached = std::cell::Cell::new(false);

        let error =
            create_frozen_private_directory_with(&destination, b".setup-failure-test-", deadline, |checkpoint, _| {
                if checkpoint == rejected {
                    reached.set(true);
                    Err(io::Error::other(format!("injected failure at {checkpoint:?}")).into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(reached.get(), "injection did not reach {rejected:?}: {error}");
        assert!(
            fs::read_dir(temporary.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".setup-failure-test-")),
            "{rejected:?} left a provisional wrapper: {error}"
        );
    }
}

#[test]
fn frozen_private_directory_normalizes_setgid_inherited_from_its_parent() {
    let temporary = tempfile::tempdir().unwrap();
    let namespace = temporary.path().join("namespace");
    fs::create_dir(&namespace).unwrap();
    fs::set_permissions(&namespace, Permissions::from_mode(0o2770)).unwrap();
    assert_ne!(fs::symlink_metadata(&namespace).unwrap().mode() & nix::libc::S_ISGID, 0);
    let destination = frozen_publication_destination(&namespace, "published");
    let deadline = Instant::now() + Duration::from_secs(10);
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();

    let directory = create_frozen_private_directory(&destination, b".setgid-test-", deadline).unwrap();
    assert_eq!(directory.file.metadata().unwrap().mode() & 0o7777, 0o700);
    remove_empty_frozen_private_directory(&directory, &destination, deadline).unwrap();
}

#[test]
fn frozen_publication_error_before_rename_preserves_the_retained_stage_for_bounded_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();

    let error = publish_frozen_root_with(&stage, &destination, &root, root_anchor, deadline, |_, _, _, _| {
        Err(io::Error::from_raw_os_error(nix::libc::EIO))
    })
    .unwrap_err();
    assert!(matches!(error, Error::PublishFrozenRoot { .. }));
    assert_eq!(
        fs::read(stage.path.join("root/candidate")).unwrap(),
        b"retained candidate"
    );
    assert!(!destination.root_path.exists());

    discard_retained_frozen_stage(&stage, &destination, &root, deadline).unwrap();
    remove_empty_frozen_private_directory(&stage, &destination, deadline).unwrap();
    assert!(!stage.path.exists());
}

#[test]
fn frozen_publication_reconciles_a_racing_destination_without_replacing_it() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
    let winner = destination.root_path.clone();

    let error = publish_frozen_root_with(
        &stage,
        &destination,
        &root,
        root_anchor,
        deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            fs::create_dir(&winner)?;
            fs::write(winner.join("winner"), b"foreign winner")?;
            renameat2_noreplace_until(
                source_directory.file(),
                source_name,
                destination_directory.file(),
                destination_name,
                deadline,
            )
        },
    )
    .unwrap_err();
    assert!(matches!(error, Error::FrozenRootDestinationExists(path) if path == winner));
    assert_eq!(fs::read(winner.join("winner")).unwrap(), b"foreign winner");
    assert_eq!(
        fs::read(stage.path.join("root/candidate")).unwrap(),
        b"retained candidate"
    );
    discard_retained_frozen_stage(&stage, &destination, &root, deadline).unwrap();
    remove_empty_frozen_private_directory(&stage, &destination, deadline).unwrap();
    assert_eq!(fs::read(winner.join("winner")).unwrap(), b"foreign winner");
}

#[test]
fn frozen_publication_detects_destination_substitution_and_never_deletes_the_foreign_tree() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
    let public = destination.root_path.clone();
    let displaced = temporary.path().join("displaced-retained-root");

    let error = publish_frozen_root_with(
        &stage,
        &destination,
        &root,
        root_anchor,
        deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            renameat2_noreplace_until(
                source_directory.file(),
                source_name,
                destination_directory.file(),
                destination_name,
                deadline,
            )?;
            fs::rename(&public, &displaced)?;
            fs::create_dir(&public)?;
            fs::write(public.join("foreign"), b"must survive")?;
            Ok(())
        },
    )
    .unwrap_err();
    assert!(matches!(error, Error::FrozenPublicationNamespaceMismatch { .. }));
    assert_eq!(fs::read(public.join("foreign")).unwrap(), b"must survive");
    assert_eq!(fs::read(displaced.join("candidate")).unwrap(), b"retained candidate");
    assert!(discard_retained_frozen_stage(&stage, &destination, &root, deadline).is_err());
    assert_eq!(fs::read(public.join("foreign")).unwrap(), b"must survive");
    remove_empty_frozen_private_directory(&stage, &destination, deadline).unwrap();
}

#[test]
fn frozen_publication_rejects_a_foreign_stage_name_without_publishing_or_deleting_it() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
    let displaced = temporary.path().join("displaced-intended-root");
    fs::rename(stage.path.join("root"), &displaced).unwrap();
    fs::create_dir(stage.path.join("root")).unwrap();
    fs::write(stage.path.join("root/foreign"), b"must survive").unwrap();

    let error = publish_frozen_root(&stage, &destination, &root, root_anchor, deadline).unwrap_err();
    assert!(matches!(error, Error::FrozenPublicationNamespaceMismatch { .. }));
    assert!(!destination.root_path.exists());
    assert_eq!(fs::read(stage.path.join("root/foreign")).unwrap(), b"must survive");
    assert_eq!(fs::read(displaced.join("candidate")).unwrap(), b"retained candidate");
    assert!(discard_retained_frozen_stage(&stage, &destination, &root, deadline).is_err());
    assert_eq!(fs::read(stage.path.join("root/foreign")).unwrap(), b"must survive");
}

#[test]
fn frozen_destination_lock_serializes_cooperating_publishers_with_a_finite_wait() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, _stage, _root, _root_anchor, deadline) = frozen_publication_fixture(temporary.path());
    let _first = lock_frozen_destination_until(&destination, deadline).unwrap();
    let second_parent = open_frozen_destination_parent(temporary.path()).unwrap();
    let second = FrozenRootDestination {
        root_path: destination.root_path.clone(),
        parent_path: destination.parent_path.clone(),
        name: destination.name.clone(),
        parent_identity: frozen_root_identity(&second_parent, temporary.path()).unwrap(),
        parent: second_parent,
    };

    let error = lock_frozen_destination_until(&second, Instant::now() + Duration::from_millis(20)).unwrap_err();
    assert!(matches!(error, Error::FrozenMaterializationTimeout { .. }));
}
