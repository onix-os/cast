#[test]
fn root_abi_entry_open_distinguishes_absence_and_pins_symlink_itself() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let directory = open_root_abi_directory(root).unwrap();
    assert!(open_root_abi_entry(&directory, root, "bin").unwrap().is_none());

    symlink("usr/bin", root.join("bin")).unwrap();
    let entry = open_root_abi_entry(&directory, root, "bin").unwrap().unwrap();
    assert!(entry.metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        read_root_abi_symlink(&entry, &root.join("bin")).unwrap().as_bytes(),
        b"usr/bin"
    );
}

#[test]
fn root_abi_links_create_only_absent_names_and_canonical_noop_is_inode_stable_and_synced() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();

    create_root_links(root).unwrap();
    assert_root_abi_links(root);
    let identities = ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)));

    let mut syncs = 0;
    create_root_links_with(
        root,
        |_| {},
        |directory| {
            syncs += 1;
            directory.sync_all()
        },
    )
    .unwrap();
    assert_eq!(syncs, 1, "an idempotent no-op must still fsync the root directory");
    assert_root_abi_links(root);
    assert_eq!(
        identities,
        ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target))),
        "canonical dangling links must be accepted without replacement"
    );
}

#[test]
fn root_abi_links_reject_wrong_dangling_and_non_utf8_targets_for_every_final_name() {
    let targets = [
        OsString::from("usr/wrong-live"),
        OsString::from("usr/wrong-dangling"),
        OsString::from_vec(b"usr/wrong-\xff".to_vec()),
    ];
    for (source, target) in ROOT_ABI_LINKS {
        for actual in &targets {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path();
            fs::create_dir_all(root.join("usr")).unwrap();
            fs::write(root.join("usr/wrong-live"), b"live").unwrap();
            symlink(actual, root.join(target)).unwrap();
            let identity = root_abi_inode(&root.join(target));

            let error = create_root_links(root).unwrap_err();
            assert!(matches!(
                error,
                Error::RootAbiLinkTargetConflict {
                    path,
                    expected,
                    actual: found,
                } if path == root.join(target)
                    && expected == source
                    && found.as_bytes() == actual.as_bytes()
            ));
            assert_eq!(root_abi_inode(&root.join(target)), identity);
            assert_eq!(
                fs::read_link(root.join(target)).unwrap().as_os_str().as_bytes(),
                actual.as_bytes()
            );
            for (_, other) in ROOT_ABI_LINKS {
                if other != target {
                    assert_root_abi_absent(&root.join(other));
                }
            }
        }
    }
}

fn assert_root_abi_type_conflict(actual_type: &'static str, setup: impl FnOnce(&Path)) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let path = root.join("bin");
    setup(&path);
    let identity = root_abi_inode(&path);

    let error = create_root_links(root).unwrap_err();
    assert!(matches!(
        error,
        Error::RootAbiLinkTypeConflict {
            path: found,
            target,
            actual_type: found_type,
        } if found == path && target == "usr/bin" && found_type == actual_type
    ));
    assert_eq!(root_abi_inode(&path), identity);
    assert_root_abi_absent(&root.join("sbin"));
}

#[test]
fn root_abi_links_reject_regular_directory_fifo_and_socket_without_mutation() {
    assert_root_abi_type_conflict("regular file", |path| fs::write(path, b"foreign").unwrap());
    assert_root_abi_type_conflict("directory", |path| {
        fs::create_dir(path).unwrap();
        fs::write(path.join("marker"), b"foreign").unwrap();
    });
    assert_root_abi_type_conflict("fifo", |path| {
        nix::unistd::mkfifo(path, Mode::from_bits_truncate(0o600)).unwrap();
    });

    // Some test sandboxes prohibit AF_UNIX creation. Regular files,
    // directories, and FIFOs above always exercise the non-symlink path;
    // exercise its socket classification whenever the host permits the
    // fixture rather than treating a capability denial as success.
    let socket_root = tempfile::tempdir().unwrap();
    let socket = socket_root.path().join("bin");
    match UnixListener::bind(&socket) {
        Ok(listener) => {
            drop(listener);
            let identity = root_abi_inode(&socket);
            let error = create_root_links(socket_root.path()).unwrap_err();
            assert!(matches!(
                error,
                Error::RootAbiLinkTypeConflict {
                    path,
                    target,
                    actual_type: "socket",
                } if path == socket && target == "usr/bin"
            ));
            assert_eq!(root_abi_inode(&socket), identity);
            assert_root_abi_absent(&socket_root.path().join("sbin"));
        }
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
        Err(error) => panic!("create root ABI socket conflict fixture: {error}"),
    }
}

#[test]
fn root_abi_links_reject_every_legacy_next_name_without_cleanup_or_partial_creation() {
    for (_, target) in ROOT_ABI_LINKS {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let next = root.join(format!("{target}.next"));
        fs::write(&next, b"foreign stage").unwrap();
        let identity = root_abi_inode(&next);

        let error = create_root_links(root).unwrap_err();
        assert!(matches!(
            error,
            Error::RootAbiStagingConflict {
                path,
                actual_type: "regular file",
                symlink_target: None,
            } if path == next
        ));
        assert_eq!(root_abi_inode(&next), identity);
        assert_eq!(fs::read(&next).unwrap(), b"foreign stage");
        for (_, final_name) in ROOT_ABI_LINKS {
            assert_root_abi_absent(&root.join(final_name));
        }
    }

    for actual in [OsString::from("usr/bin"), OsString::from_vec(b"usr/\xff".to_vec())] {
        let temporary = tempfile::tempdir().unwrap();
        let next = temporary.path().join("bin.next");
        symlink(&actual, &next).unwrap();
        let identity = root_abi_inode(&next);
        let error = create_root_links(temporary.path()).unwrap_err();
        assert!(matches!(
            error,
            Error::RootAbiStagingConflict {
                path,
                actual_type: "symlink",
                symlink_target: Some(found),
            } if path == next && found.as_bytes() == actual.as_bytes()
        ));
        assert_eq!(root_abi_inode(&next), identity);
    }
}

#[test]
fn root_abi_links_authenticate_absent_name_races_without_overwriting() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let raced = root.join("sbin");
    create_root_links_with(
        root,
        |checkpoint| {
            if checkpoint == RootAbiLinkCheckpoint::PreflightComplete {
                fs::write(&raced, b"raced foreign entry").unwrap();
            }
        },
        |directory| directory.sync_all(),
    )
    .unwrap_err();
    assert_eq!(fs::read(&raced).unwrap(), b"raced foreign entry");
    assert_root_abi_absent(&root.join("bin"));

    let exact = tempfile::tempdir().unwrap();
    create_root_links_with(
        exact.path(),
        |checkpoint| {
            if checkpoint == RootAbiLinkCheckpoint::PreflightComplete {
                symlink("usr/sbin", exact.path().join("sbin")).unwrap();
            }
        },
        |directory| directory.sync_all(),
    )
    .unwrap();
    assert_root_abi_links(exact.path());
}

#[test]
fn root_abi_links_leave_raced_next_and_exact_partial_links_retry_safe() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let raced = root.join("sbin.next");
    let error = create_root_links_with(
        root,
        |checkpoint| {
            if checkpoint == RootAbiLinkCheckpoint::PreflightComplete {
                fs::write(&raced, b"raced stage").unwrap();
            }
        },
        |directory| directory.sync_all(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::RootAbiStagingConflict { path, .. } if path == raced));
    assert_eq!(fs::read(&raced).unwrap(), b"raced stage");
    assert_root_abi_links_except_next(root, "sbin.next");
    let identities = ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)));

    fs::remove_file(&raced).unwrap();
    create_root_links(root).unwrap();
    assert_root_abi_links(root);
    assert_eq!(
        identities,
        ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)))
    );
}

fn assert_root_abi_links_except_next(root: &Path, allowed_next: &str) {
    for (source, target) in ROOT_ABI_LINKS {
        assert_eq!(
            fs::read_link(root.join(target)).unwrap().as_os_str().as_bytes(),
            source.as_bytes()
        );
        let next = format!("{target}.next");
        if next != allowed_next {
            assert_root_abi_absent(&root.join(next));
        }
    }
}

#[test]
fn root_abi_links_sync_failure_is_retryable_without_replacing_exact_links() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let error = create_root_links_with(
        root,
        |_| {},
        |_| Err(io::Error::other("injected root directory sync failure")),
    )
    .unwrap_err();
    assert!(matches!(error, Error::SyncRootAbiDirectory { .. }));
    assert_root_abi_links(root);
    let identities = ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)));

    create_root_links(root).unwrap();
    assert_eq!(
        identities,
        ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)))
    );
}

#[test]
fn root_abi_links_revalidate_post_sync_name_races_without_repairing_them() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let bin = root.join("bin");
    let error = create_root_links_with(
        root,
        |checkpoint| {
            if checkpoint == RootAbiLinkCheckpoint::AfterSync {
                fs::remove_file(&bin).unwrap();
                symlink("usr/wrong-after-sync", &bin).unwrap();
            }
        },
        |directory| directory.sync_all(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::RootAbiLinkTargetConflict {
            path,
            expected,
            actual,
        } if path == bin && expected == "usr/bin" && actual.as_bytes() == b"usr/wrong-after-sync"
    ));
    assert_eq!(fs::read_link(&bin).unwrap(), Path::new("usr/wrong-after-sync"));
}

#[test]
fn root_abi_links_reject_exact_target_aba_across_sync_and_preserve_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let bin = root.join("bin");
    let mut replacement = None;
    let error = create_root_links_with(
        root,
        |checkpoint| {
            if checkpoint == RootAbiLinkCheckpoint::AfterSync {
                fs::remove_file(&bin).unwrap();
                symlink("usr/bin", &bin).unwrap();
                replacement = Some(root_abi_inode(&bin));
            }
        },
        |directory| directory.sync_all(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::RootAbiLinkReplaced(path) if path == bin));
    assert_eq!(root_abi_inode(&bin), replacement.unwrap());
    assert_eq!(fs::read_link(&bin).unwrap(), Path::new("usr/bin"));
}

#[test]
fn root_abi_links_detect_public_root_replacement_and_never_touch_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let detached = temporary.path().join("detached");
    fs::create_dir(&root).unwrap();

    let error = create_root_links_with(
        &root,
        |checkpoint| {
            if checkpoint == RootAbiLinkCheckpoint::RootOpened {
                fs::rename(&root, &detached).unwrap();
                fs::create_dir(&root).unwrap();
                fs::write(root.join("replacement-marker"), b"replacement").unwrap();
            }
        },
        |directory| directory.sync_all(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::RootAbiDirectoryReplaced(path) if path == root));
    assert_eq!(fs::read(root.join("replacement-marker")).unwrap(), b"replacement");
    assert_root_abi_absent(&root.join("bin"));
    assert_root_abi_links(&detached);
}

#[test]
fn root_abi_links_reject_terminal_and_intermediate_root_symlinks_before_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let real = temporary.path().join("real");
    fs::create_dir(&real).unwrap();
    let alias = temporary.path().join("alias");
    symlink(&real, &alias).unwrap();
    assert!(matches!(
        create_root_links(&alias),
        Err(Error::OpenRootAbiDirectory { root, .. }) if root == alias
    ));
    assert_root_abi_absent(&real.join("bin"));

    let child = real.join("child");
    fs::create_dir(&child).unwrap();
    let through_alias = alias.join("child");
    assert!(matches!(
        create_root_links(&through_alias),
        Err(Error::OpenRootAbiDirectory { root, .. }) if root == through_alias
    ));
    assert_root_abi_absent(&child.join("bin"));
}
