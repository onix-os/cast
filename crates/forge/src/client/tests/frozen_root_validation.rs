#[test]
fn frozen_root_normalizes_and_discards_a_mode_zero_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    let client = Client::frozen(
        "frozen-mode-zero-directory-test",
        frozen_test_installation(&installation_root),
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    fs::create_dir_all(client.installation.assets_path("v2")).unwrap();
    let package = package::Id::from("mode-zero-directory-package");
    client
        .layout_db
        .add(
            &package,
            &StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFDIR,
                tag: 0,
                file: StonePayloadLayoutFile::Directory("locked".into()),
            },
        )
        .unwrap();

    let _materialized = client
        .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
        .unwrap();
    assert_eq!(
        fs::symlink_metadata(frozen_root.join("usr/locked"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0
    );
    client.discard_frozen_root().unwrap();
    assert!(!frozen_root.exists());
}

#[test]
fn frozen_root_rejects_unenforceable_ownership_before_touching_destination() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let blit_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&blit_root).unwrap();
    let marker = blit_root.join("untouched");
    fs::write(&marker, b"original root").unwrap();
    let installation = frozen_test_installation(&installation_root);
    let client = Client::frozen(
        "frozen-ownership-test",
        installation,
        repository::Map::default(),
        &blit_root,
    )
    .unwrap();
    let package = package::Id::from("owned-by-another-user");
    client
        .layout_db
        .add(
            &package,
            &StonePayloadLayoutRecord {
                uid: 1000,
                gid: 1000,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(1, "share/owned".into()),
            },
        )
        .unwrap();

    let error = client
        .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnsupportedFrozenOwnership {
            package: found,
            path,
            uid: 1000,
            gid: 1000,
        } if found == package && path == "/usr/share/owned"
    ));
    assert_eq!(fs::read(marker).unwrap(), b"original root");
}

fn assert_frozen_layout_rejected_before_touching_destination(
    layout: StonePayloadLayoutRecord,
    assert_error: impl FnOnce(Error),
) {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let blit_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&blit_root).unwrap();
    let marker = blit_root.join("untouched");
    fs::write(&marker, b"original root").unwrap();
    let installation = frozen_test_installation(&installation_root);
    let client = Client::frozen(
        "frozen-invalid-layout-test",
        installation,
        repository::Map::default(),
        &blit_root,
    )
    .unwrap();
    let package = package::Id::from("invalid-layout");
    client.layout_db.add(&package, &layout).unwrap();

    assert_error(
        client
            .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
            .unwrap_err(),
    );
    assert_eq!(fs::read(marker).unwrap(), b"original root");
}

#[test]
fn frozen_consumers_reject_absolute_raw_stone_targets_without_a_compatibility_spelling() {
    let layout = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(1, "/usr/bin/tool".into()),
    };

    assert_frozen_layout_rejected_before_touching_destination(layout.clone(), |error| {
        assert!(matches!(
            error,
            Error::InvalidStoneLayoutTarget {
                package,
                target,
                reason: "the target is absolute",
            } if package == package::Id::from("invalid-layout")
                && target == "/usr/bin/tool"
        ));
    });

    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&frozen_root).unwrap();
    let client = Client::frozen(
        "frozen-invalid-executable-layout-test",
        frozen_test_installation(&installation_root),
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    let package = package::Id::from("absolute-executable-layout");
    client.layout_db.add(&package, &layout).unwrap();
    let binding = FrozenExecutableBinding {
        package: package.clone(),
        path: PathBuf::from("/usr/bin/tool"),
    };

    assert!(matches!(
        client.require_frozen_executables(
            std::slice::from_ref(&package),
            std::slice::from_ref(&binding),
        ),
        Err(Error::InvalidStoneLayoutTarget {
            package: rejected_package,
            target,
            reason: "the target is absolute",
        }) if rejected_package == package && target == "/usr/bin/tool"
    ));
}

#[test]
fn direct_database_frozen_consumer_rejects_reserved_targets_before_destination_mutation() {
    for target in [
        ".cast-state-id.tmp",
        ".cast-tree-id",
        ".cast-tree-id.tmp",
        ".stateID/forged-child",
        "lib/os-release",
        "lib/os-release/forged-child",
        "lib/system-model.glu",
        "lib/system-model.glu/forged-child",
    ] {
        let layout = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, target.into()),
        };

        assert_frozen_layout_rejected_before_touching_destination(layout, |error| {
            assert!(matches!(
                error,
                Error::InvalidStoneLayoutTarget {
                    package,
                    target: rejected_target,
                    reason: "the target is reserved for Cast system metadata",
                } if package == package::Id::from("invalid-layout")
                    && rejected_target == target
            ));
        });
    }
}

#[test]
fn frozen_root_rejects_inconsistent_or_unenforceable_modes_before_touching_destination() {
    let cases = [
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFDIR | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, "share/type-mismatch".into()),
        },
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o777 | (1 << 31),
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, "share/unsupported-mode-bit".into()),
        },
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFLNK | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Symlink("target".into(), "share/symlink-mode".into()),
        },
    ];

    for layout in cases {
        assert_frozen_layout_rejected_before_touching_destination(layout, |error| {
            assert!(matches!(error, Error::InvalidFrozenLayoutMode { .. }));
        });
    }
}

#[test]
fn frozen_root_rejects_empty_and_nul_symlink_targets_before_touching_destination() {
    for (target, expected_reason) in [("", "the target is empty"), ("bad\0target", "the target contains NUL")] {
        assert_frozen_layout_rejected_before_touching_destination(
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFLNK | 0o777,
                tag: 0,
                file: StonePayloadLayoutFile::Symlink(target.into(), "share/link".into()),
            },
            |error| {
                assert!(matches!(
                    error,
                    Error::InvalidFrozenLayoutSymlinkTarget { package, reason }
                        if package == package::Id::from("invalid-layout")
                            && reason == expected_reason
                ));
            },
        );
    }
}

#[test]
fn frozen_root_rejects_nul_paths_before_touching_destination() {
    assert_frozen_layout_rejected_before_touching_destination(
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, "share/nul\0path".into()),
        },
        |error| {
            assert!(matches!(
                error,
                Error::InvalidStoneLayoutTarget {
                    package,
                    target,
                    reason: "the target contains an ASCII control byte",
                } if package == package::Id::from("invalid-layout")
                    && target == "share/nul\0path"
            ));
        },
    );
}

fn frozen_path_with_components(components: usize) -> String {
    assert!(components >= 1);
    let mut path = String::from("/usr");
    for _ in 1..components {
        path.push_str("/a");
    }
    path
}

#[test]
fn frozen_layout_path_policy_accepts_exact_limits_and_rejects_n_plus_one() {
    let exact_bytes = format!("/usr/{}", "a".repeat(MAX_FROZEN_EXECUTABLE_PATH_BYTES - "/usr/".len()));
    assert_eq!(exact_bytes.len(), MAX_FROZEN_EXECUTABLE_PATH_BYTES);
    assert!(require_materialized_frozen_path_policy(&exact_bytes).is_ok());
    let oversized = format!("{exact_bytes}a");
    assert!(matches!(
        require_materialized_frozen_path_policy(&oversized),
        Err(FrozenLayoutPathPolicyError::TooLong { actual })
            if actual == MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1
    ));

    let exact_depth = frozen_path_with_components(MAX_FROZEN_LAYOUT_PATH_COMPONENTS);
    assert!(require_materialized_frozen_path_policy(&exact_depth).is_ok());
    let excessive_depth = frozen_path_with_components(MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1);
    assert!(matches!(
        require_materialized_frozen_path_policy(&excessive_depth),
        Err(FrozenLayoutPathPolicyError::TooDeep { actual })
            if actual == MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1
    ));

    let symlink_layout = |target: String| StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFLNK | 0o777,
        tag: 0,
        file: StonePayloadLayoutFile::Symlink(target.into(), "share/link".into()),
    };
    assert!(
        FrozenLayoutEntry::new(
            package::Id::from("exact-target"),
            symlink_layout("a".repeat(MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES)),
            0,
        )
        .is_ok()
    );
    assert!(matches!(
        FrozenLayoutEntry::new(
            package::Id::from("oversized-target"),
            symlink_layout("a".repeat(MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1)),
            0,
        ),
        Err(Error::FrozenLayoutSymlinkTargetTooLong { limit, actual, .. })
            if limit == MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES
                && actual == MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1
    ));
}

#[test]
fn frozen_root_rejects_oversized_paths_targets_and_depth_before_touching_destination() {
    let oversized_path = "a".repeat(MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1 - "/usr/".len());
    assert_frozen_layout_rejected_before_touching_destination(
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, oversized_path.into()),
        },
        |error| {
            assert!(matches!(
                error,
                Error::InvalidStoneLayoutTarget {
                    package,
                    reason: "the materialized path exceeds Linux PATH_MAX",
                    ..
                } if package == package::Id::from("invalid-layout")
            ));
        },
    );

    assert_frozen_layout_rejected_before_touching_destination(
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFLNK | 0o777,
            tag: 0,
            file: StonePayloadLayoutFile::Symlink(
                "a".repeat(MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1).into(),
                "share/oversized-target".into(),
            ),
        },
        |error| {
            assert!(matches!(
                error,
                Error::FrozenLayoutSymlinkTargetTooLong { limit, actual, .. }
                    if limit == MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES
                        && actual == MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1
            ));
        },
    );

    assert_frozen_layout_rejected_before_touching_destination(
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(
                1,
                std::iter::repeat_n("a", MAX_FROZEN_LAYOUT_PATH_COMPONENTS)
                    .collect::<Vec<_>>()
                    .join("/")
                    .into(),
            ),
        },
        |error| {
            assert!(matches!(
                error,
                Error::InvalidStoneLayoutTarget {
                    package,
                    reason: "the materialized path is too deep",
                    ..
                } if package == package::Id::from("invalid-layout")
            ));
        },
    );
}

#[test]
fn frozen_materializer_implicit_directory_limits_accept_n_and_reject_n_plus_one() {
    let package = package::Id::from("implicit-directory-budget");
    let entry = FrozenLayoutEntry::new(
        package,
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, "a/b".into()),
        },
        0,
    )
    .unwrap();
    let entries = [entry];
    let exact_bytes = "/usr/a".len() + "/usr".len() + "/".len();

    validate_frozen_tree_collisions_with_limits(&entries, 3, exact_bytes).unwrap();
    assert!(matches!(
        validate_frozen_tree_collisions_with_limits(&entries, 2, usize::MAX),
        Err(Error::FrozenExecutableDirectoryLimit { limit: 2, actual: 3 })
    ));
    assert!(matches!(
        validate_frozen_tree_collisions_with_limits(&entries, 3, exact_bytes - 1),
        Err(Error::FrozenExecutableDirectoryByteLimit { limit, actual })
            if limit == exact_bytes - 1 && actual == exact_bytes
    ));
}

#[test]
fn frozen_root_rejects_conflicting_duplicate_directory_metadata() {
    let first = package::Id::from("a-directory-owner");
    let second = package::Id::from("z-directory-owner");
    let first_layout = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFDIR | 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Directory("share/collision".into()),
    };
    let second_layout = StonePayloadLayoutRecord {
        mode: nix::libc::S_IFDIR | 0o700,
        ..first_layout.clone()
    };

    let error = frozen_vfs(
        &[first.clone(), second.clone()],
        vec![(second.clone(), second_layout), (first.clone(), first_layout)],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::FrozenPathCollision { path, first: found_first, second: found_second }
            if path == "/usr/share/collision" && found_first == first && found_second == second
    ));
}

#[test]
fn frozen_root_rejects_an_explicit_child_beneath_a_non_directory_parent() {
    let parent_package = package::Id::from("a-file-parent");
    let child_package = package::Id::from("z-file-child");
    let regular = |digest, path: &str| StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o644,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(digest, path.into()),
    };

    let error = frozen_vfs(
        &[parent_package.clone(), child_package.clone()],
        vec![
            (parent_package.clone(), regular(1, "share/file")),
            (child_package.clone(), regular(2, "share/file/child")),
        ],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::FrozenPathCollision { path, first, second }
            if path == "/usr/share/file/child"
                && first == parent_package
                && second == child_package
    ));
}

#[test]
fn frozen_root_rejects_descendants_beneath_directory_symlink_redirects_outside_usr() {
    let package = package::Id::from("redirect-escape");
    let link = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFLNK | 0o777,
        tag: 0,
        file: StonePayloadLayoutFile::Symlink("/".into(), "escape".into()),
    };
    let child = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o644,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(1, "escape/etc/passwd".into()),
    };

    let error = frozen_vfs(
        std::slice::from_ref(&package),
        vec![(package.clone(), link), (package.clone(), child)],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::FrozenDirectorySymlinkDescendant { package: found, path, redirect }
            if found == package
                && path.as_ref() == "/usr/escape/etc/passwd"
                && redirect.as_ref() == "/usr/escape"
    ));
}

#[test]
fn frozen_root_rejects_arbitrary_descendants_beneath_directory_symlinks_before_materializing() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&frozen_root).unwrap();
    let marker = frozen_root.join("untouched");
    fs::write(&marker, b"original root").unwrap();
    let client = Client::frozen(
        "frozen-directory-redirect-test",
        frozen_test_installation(&installation_root),
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    let package = package::Id::from("redirected-data-package");
    let directory = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFDIR | 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Directory("real".into()),
    };
    let redirect = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFLNK | 0o777,
        tag: 0,
        file: StonePayloadLayoutFile::Symlink("/usr/real".into(), "alias".into()),
    };
    let data = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o644,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(1, "alias/data".into()),
    };
    client
        .layout_db
        .batch_add([(&package, &directory), (&package, &redirect), (&package, &data)])
        .unwrap();

    let error = client
        .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_123)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::FrozenDirectorySymlinkDescendant { package: found, path, redirect }
            if found == package
                && path.as_ref() == "/usr/alias/data"
                && redirect.as_ref() == "/usr/alias"
    ));
    assert_eq!(fs::read(marker).unwrap(), b"original root");
    assert!(!frozen_root.join("usr").exists());
}

#[test]
fn frozen_root_rejects_conflicting_directory_metadata_after_redirect() {
    let first = package::Id::from("a-redirected-directory");
    let second = package::Id::from("z-real-directory");
    let directory = |mode: u32, target: &str| StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFDIR | mode,
        tag: 0,
        file: StonePayloadLayoutFile::Directory(target.into()),
    };
    let link = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFLNK | 0o777,
        tag: 0,
        file: StonePayloadLayoutFile::Symlink("/usr/real".into(), "alias".into()),
    };

    let error = frozen_vfs(
        &[first.clone(), second.clone()],
        vec![
            (first.clone(), link),
            (first.clone(), directory(0o700, "alias/shared")),
            (second.clone(), directory(0o755, "real")),
            (second.clone(), directory(0o755, "real/shared")),
        ],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::FrozenDirectorySymlinkDescendant { package: found, path, redirect }
            if found == first
                && path.as_ref() == "/usr/alias/shared"
                && redirect.as_ref() == "/usr/alias"
    ));
}

#[test]
fn verify_reblits_and_preserves_the_existing_normalized_snapshot() {
    const CHILD: &str = "CAST_VERIFY_REPAIR_TIMEOUT_CHILD";
    const TEST: &str = "client::tests::verify_reblits_and_preserves_the_existing_normalized_snapshot";

    if std::env::var_os(CHILD).is_none() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg(TEST)
            .arg("--exact")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(CHILD, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if child.try_wait().unwrap().is_some() {
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "verify repair child failed\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "verify repair exceeded 15 seconds (possible coordinator deadlock)\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    let temporary = tempfile::tempdir().unwrap();
    let mut client = stateful_test_client(temporary.path());
    fs::create_dir_all(client.installation.root.join("etc")).unwrap();
    fs::set_permissions(client.installation.root.join("etc"), Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(client.installation.assets_path("v2")).unwrap();

    let package = package::Id::from("verify-package");
    let layout = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFDIR | 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Directory("share/verify-proof".into()),
    };
    client.layout_db.add(&package, &layout).unwrap();
    let state = client
        .state_db
        .add(&[Selection::explicit(package)], Some("active"), None)
        .unwrap();
    client.installation.active_state = Some(state.id);
    record_state_id(&client.installation.root, state.id).unwrap();

    let original = generated_system_snapshot("active-package");
    let expected = original.encoded().to_owned();
    record_system_snapshot(&client.installation.root, original).unwrap();
    let restored_path = client.installation.root.join("usr/share/verify-proof");
    assert!(!restored_path.exists());

    client.verify(true, false).unwrap();

    assert!(restored_path.is_dir());
    assert_generated_snapshot(
        &system_model::snapshot_path(&client.installation.root),
        &expected,
        "active-package",
    );
}
