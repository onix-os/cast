#[test]
fn frozen_discard_widens_unreadable_roots_for_detach_and_private_cleanup() {
    for mode in [0o000, 0o300] {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
        fs::set_permissions(&destination.root_path, Permissions::from_mode(mode)).unwrap();

        discard_frozen_root_destination_until(&destination, deadline).unwrap();

        assert!(!destination.root_path.exists(), "root mode was {mode:04o}");
        assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
    }
}

#[test]
fn frozen_discard_restores_mode_when_post_chmod_identity_inspection_fails() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
    fs::set_permissions(&destination.root_path, Permissions::from_mode(0o000)).unwrap();
    let expected = frozen_root_identity(&pinned, &destination.root_path).unwrap();

    let error = prepare_frozen_discard_root_mode_with(&pinned, &destination, expected, deadline, |_, path| {
        Err(Error::StatFrozenExecutableRoot {
            path: path.to_owned(),
            source: io::Error::from_raw_os_error(nix::libc::EIO),
        })
    })
    .unwrap_err();

    assert!(matches!(error, Error::StatFrozenExecutableRoot { .. }));
    assert_eq!(frozen_root_identity(&pinned, &destination.root_path).unwrap(), expected);
    assert_eq!(
        fs::symlink_metadata(&destination.root_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0
    );
    fs::set_permissions(&destination.root_path, Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn frozen_discard_is_idempotent_when_the_public_root_is_absent() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = frozen_publication_destination(temporary.path(), "published");
    let deadline = Instant::now() + Duration::from_secs(30);

    discard_frozen_root_destination_until(&destination, deadline).unwrap();
    discard_frozen_root_destination_until(&destination, deadline).unwrap();

    assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
}

#[test]
fn frozen_discard_unlinks_symlinks_without_touching_external_targets() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
    let outside = temporary.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("must-survive"), b"external").unwrap();
    symlink(&outside, destination.root_path.join("escape")).unwrap();

    discard_frozen_root_destination_until(&destination, deadline).unwrap();

    assert_eq!(fs::read(outside.join("must-survive")).unwrap(), b"external");
    assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
}

#[test]
fn frozen_discard_depth_limit_accepts_n_and_preserves_n_plus_one_privately() {
    for depth in [MAX_FROZEN_LAYOUT_PATH_COMPONENTS, MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1] {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
        let mut nested = destination.root_path.clone();
        for _ in 0..depth {
            nested.push("d");
            fs::create_dir(&nested).unwrap();
        }

        let result = discard_frozen_root_destination_until(&destination, deadline);
        if depth == MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
            result.unwrap();
            assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
        } else {
            assert!(matches!(
                result,
                Err(Error::FrozenDiscardDepthLimit { limit, actual })
                    if limit == MAX_FROZEN_LAYOUT_PATH_COMPONENTS
                        && actual == MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1
            ));
            assert!(!destination.root_path.exists());
            assert_eq!(frozen_discard_quarantine_names(temporary.path()).len(), 1);
        }
    }
}

#[test]
fn frozen_discard_entry_limit_rejects_n_plus_one_before_deletion() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
    let root = fs::File::open(&destination.root_path).unwrap();
    let mut entries = MAX_FROZEN_NORMALIZED_INODES;

    let error = discard_frozen_directory(&root, &destination.root_path, 0, &mut entries, deadline).unwrap_err();

    assert!(matches!(
        error,
        Error::FrozenDiscardEntryLimit { limit, actual }
            if limit == MAX_FROZEN_NORMALIZED_INODES && actual == MAX_FROZEN_NORMALIZED_INODES + 1
    ));
    assert_eq!(
        fs::read(destination.root_path.join("candidate")).unwrap(),
        b"retained candidate"
    );
}

#[test]
fn frozen_discard_rejects_non_directory_roots_without_creating_quarantine() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = frozen_publication_destination(temporary.path(), "published");
    let deadline = Instant::now() + Duration::from_secs(30);
    fs::write(&destination.root_path, b"must survive").unwrap();

    let error = discard_frozen_root_destination_until(&destination, deadline).unwrap_err();
    assert!(matches!(error, Error::UnsafeFrozenRootDiscard { .. }));
    assert_eq!(fs::read(&destination.root_path).unwrap(), b"must survive");
    assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());

    fs::remove_file(&destination.root_path).unwrap();
    let target = temporary.path().join("symlink-target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("must-survive"), b"foreign").unwrap();
    symlink(&target, &destination.root_path).unwrap();

    assert!(discard_frozen_root_destination_until(&destination, deadline).is_err());
    assert_eq!(fs::read(target.join("must-survive")).unwrap(), b"foreign");
    assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
}

#[test]
fn frozen_discard_rename_failure_removes_only_its_exact_empty_quarantine() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
    fs::set_permissions(&destination.root_path, Permissions::from_mode(0o000)).unwrap();

    let error = discard_frozen_root_destination_with(&destination, deadline, |_, _, _, _| {
        Err(io::Error::from_raw_os_error(nix::libc::EIO))
    })
    .unwrap_err();

    assert!(matches!(error, Error::DetachFrozenRoot { .. }));
    assert_eq!(
        fs::symlink_metadata(&destination.root_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0
    );
    fs::set_permissions(&destination.root_path, Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        fs::read(destination.root_path.join("candidate")).unwrap(),
        b"retained candidate"
    );
    assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
}

#[test]
fn frozen_discard_adopts_an_applied_detach_even_when_the_syscall_reports_error() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, pinned, expected, deadline) = frozen_discard_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
    let quarantine = create_frozen_private_directory(&destination, b".discard-applied-test-", deadline).unwrap();

    detach_frozen_root_with(
        &destination,
        &quarantine,
        &pinned,
        expected,
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

    assert!(!destination.root_path.exists());
    assert_eq!(
        fs::read(quarantine.path.join("root/candidate")).unwrap(),
        b"retained candidate"
    );
    let cleanup_deadline = frozen_namespace_recovery_deadline();
    discard_retained_frozen_stage(&quarantine, &destination, &pinned, cleanup_deadline).unwrap();
    remove_empty_frozen_private_directory(&quarantine, &destination, cleanup_deadline).unwrap();
}

#[test]
fn frozen_discard_completes_after_an_applied_detach_reports_error() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());

    discard_frozen_root_destination_with(
        &destination,
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

    assert!(!destination.root_path.exists());
    assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
}

#[test]
fn frozen_discard_reconciles_an_applied_detach_after_the_work_deadline_expires() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, pinned, expected, _) = frozen_discard_fixture(temporary.path());
    let setup_deadline = Instant::now() + Duration::from_secs(30);
    let _lock = lock_frozen_destination_until(&destination, setup_deadline).unwrap();
    let quarantine = create_frozen_private_directory(&destination, b".discard-expired-test-", setup_deadline).unwrap();
    let work_deadline = Instant::now() + Duration::from_millis(50);

    detach_frozen_root_with(
        &destination,
        &quarantine,
        &pinned,
        expected,
        work_deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            renameat2_noreplace_until(
                source_directory.file(),
                source_name,
                destination_directory.file(),
                destination_name,
                work_deadline,
            )?;
            while Instant::now() <= work_deadline {
                std::thread::yield_now();
            }
            Ok(())
        },
    )
    .unwrap();

    assert!(!destination.root_path.exists());
    assert_eq!(
        fs::read(quarantine.path.join("root/candidate")).unwrap(),
        b"retained candidate"
    );
    let cleanup_deadline = frozen_namespace_recovery_deadline();
    discard_retained_frozen_stage(&quarantine, &destination, &pinned, cleanup_deadline).unwrap();
    remove_empty_frozen_private_directory(&quarantine, &destination, cleanup_deadline).unwrap();
}

#[test]
fn frozen_discard_unlink_reconciles_applied_errors_and_bounded_interrupts() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = open_frozen_destination_parent(temporary.path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);

    for (name, report_applied_error) in [(c"applied", true), (c"interrupted", false)] {
        let path = temporary.path().join(OsStr::from_bytes(name.to_bytes()));
        fs::write(&path, b"discard me").unwrap();
        let anchor = open_frozen_named_entry_until(&directory, name, &path, deadline)
            .unwrap()
            .unwrap();
        let expected = frozen_root_identity(&anchor, &path).unwrap();
        let mut calls = 0usize;

        unlink_frozen_discard_entry_with(&directory, name, &path, expected, deadline, |directory, name| {
            calls += 1;
            if report_applied_error {
                unlinkat(Some(directory.as_raw_fd()), name, UnlinkatFlags::NoRemoveDir)?;
                Err(Errno::EIO)
            } else if calls == 1 {
                Err(Errno::EINTR)
            } else {
                unlinkat(Some(directory.as_raw_fd()), name, UnlinkatFlags::NoRemoveDir)
            }
        })
        .unwrap();

        assert!(!path.exists());
        assert_eq!(calls, if report_applied_error { 1 } else { 2 });
    }

    let bounded = temporary.path().join("bounded-interrupts");
    fs::write(&bounded, b"must survive").unwrap();
    let anchor = open_frozen_named_entry_until(&directory, c"bounded-interrupts", &bounded, deadline)
        .unwrap()
        .unwrap();
    let expected = frozen_root_identity(&anchor, &bounded).unwrap();
    let mut calls = 0usize;
    let error = unlink_frozen_discard_entry_with(
        &directory,
        c"bounded-interrupts",
        &bounded,
        expected,
        deadline,
        |_, _| {
            calls += 1;
            Err(Errno::EINTR)
        },
    )
    .unwrap_err();
    assert!(matches!(error, Error::RemoveFrozenDiscardEntry { .. }));
    assert_eq!(calls, MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS + 1);
    assert_eq!(fs::read(&bounded).unwrap(), b"must survive");
}

#[test]
fn frozen_discard_unlink_never_retries_against_a_foreign_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = open_frozen_destination_parent(temporary.path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let path = temporary.path().join("candidate");
    let displaced = temporary.path().join("displaced-candidate");
    fs::write(&path, b"retained").unwrap();
    let anchor = open_frozen_named_entry_until(&directory, c"candidate", &path, deadline)
        .unwrap()
        .unwrap();
    let expected = frozen_root_identity(&anchor, &path).unwrap();

    let error = unlink_frozen_discard_entry_with(&directory, c"candidate", &path, expected, deadline, |_, _| {
        fs::rename(&path, &displaced).unwrap();
        fs::write(&path, b"foreign").unwrap();
        Err(Errno::EIO)
    })
    .unwrap_err();

    assert!(matches!(error, Error::FrozenDiscardEntryChanged));
    assert_eq!(fs::read(&path).unwrap(), b"foreign");
    assert_eq!(fs::read(&displaced).unwrap(), b"retained");
}

#[test]
fn frozen_discard_preserves_a_racing_quarantine_collision_and_the_public_root() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, pinned, expected, deadline) = frozen_discard_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
    let quarantine = create_frozen_private_directory(&destination, b".discard-collision-test-", deadline).unwrap();
    let collision = quarantine.path.join("root");

    let error = detach_frozen_root_with(
        &destination,
        &quarantine,
        &pinned,
        expected,
        deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            fs::create_dir(&collision)?;
            fs::write(collision.join("foreign"), b"must survive")?;
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
    assert!(matches!(error, Error::FrozenDiscardNamespaceMismatch { .. }));
    assert_eq!(
        fs::read(destination.root_path.join("candidate")).unwrap(),
        b"retained candidate"
    );
    assert_eq!(fs::read(collision.join("foreign")).unwrap(), b"must survive");
}

#[test]
fn frozen_discard_detects_source_substitution_without_deleting_the_foreign_tree() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, pinned, expected, deadline) = frozen_discard_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
    let quarantine = create_frozen_private_directory(&destination, b".discard-source-test-", deadline).unwrap();
    let displaced = temporary.path().join("displaced-retained-root");
    let public = destination.root_path.clone();

    let error = detach_frozen_root_with(
        &destination,
        &quarantine,
        &pinned,
        expected,
        deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            fs::rename(&public, &displaced)?;
            fs::create_dir(&public)?;
            fs::write(public.join("foreign"), b"must survive")?;
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
    assert!(matches!(error, Error::FrozenDiscardNamespaceMismatch { .. }));
    assert_eq!(fs::read(displaced.join("candidate")).unwrap(), b"retained candidate");
    assert_eq!(fs::read(quarantine.path.join("root/foreign")).unwrap(), b"must survive");
    assert!(!public.exists());
}

#[test]
fn frozen_discard_preserves_a_replaced_quarantine_wrapper_and_the_detached_root() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
    let displaced_wrapper = temporary.path().join("displaced-discard-wrapper");

    let error = discard_frozen_root_destination_with(
        &destination,
        deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            let names = frozen_discard_quarantine_names(temporary.path());
            assert_eq!(names.len(), 1);
            let public_wrapper = temporary.path().join(&names[0]);
            fs::rename(&public_wrapper, &displaced_wrapper)?;
            fs::create_dir(&public_wrapper)?;
            fs::write(public_wrapper.join("foreign"), b"must survive")?;
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

    assert!(matches!(error, Error::CleanupFrozenDiscardQuarantine { .. }));
    assert!(!destination.root_path.exists());
    assert_eq!(
        fs::read(displaced_wrapper.join("root/candidate")).unwrap(),
        b"retained candidate"
    );
    let names = frozen_discard_quarantine_names(temporary.path());
    assert_eq!(names.len(), 1);
    assert_eq!(
        fs::read(temporary.path().join(&names[0]).join("foreign")).unwrap(),
        b"must survive"
    );
}

#[test]
fn frozen_discard_uses_the_same_finite_parent_lock_as_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
    let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();

    let error =
        discard_frozen_root_destination_until(&destination, Instant::now() + Duration::from_millis(20)).unwrap_err();
    assert!(matches!(error, Error::FrozenMaterializationTimeout { .. }));
    assert_eq!(
        fs::read(destination.root_path.join("candidate")).unwrap(),
        b"retained candidate"
    );
}

#[test]
fn frozen_discard_rejects_destination_parent_replacement_without_touching_either_tree() {
    let temporary = tempfile::tempdir().unwrap();
    let namespace = temporary.path().join("namespace");
    fs::create_dir(&namespace).unwrap();
    let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(&namespace);
    let displaced_namespace = temporary.path().join("displaced-namespace");
    fs::rename(&namespace, &displaced_namespace).unwrap();
    fs::create_dir(&namespace).unwrap();
    fs::create_dir(namespace.join("published")).unwrap();
    fs::write(namespace.join("published/foreign"), b"must survive").unwrap();

    assert!(discard_frozen_root_destination_until(&destination, deadline).is_err());
    assert_eq!(
        fs::read(displaced_namespace.join("published/candidate")).unwrap(),
        b"retained candidate"
    );
    assert_eq!(fs::read(namespace.join("published/foreign")).unwrap(), b"must survive");
}

#[test]
fn failed_frozen_root_blit_never_publishes_or_leaves_a_reusable_stage() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    let client = Client::frozen(
        "frozen-partial-stage-test",
        frozen_test_installation(&installation_root),
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    let package = package::Id::from("missing-asset-package");
    client
        .layout_db
        .batch_add([
            (
                &package,
                &StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFDIR | 0o755,
                    tag: 0,
                    file: StonePayloadLayoutFile::Directory("bin".into()),
                },
            ),
            (
                &package,
                &StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFREG | 0o755,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(42, "bin/missing".into()),
                },
            ),
        ])
        .unwrap();

    assert!(
        client
            .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
            .is_err()
    );
    assert!(!frozen_root.exists());
    let stage_count = fs::read_dir(temporary.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().as_bytes().starts_with(b".forge-frozen-stage-"))
        .count();
    assert_eq!(stage_count, 0);
}
