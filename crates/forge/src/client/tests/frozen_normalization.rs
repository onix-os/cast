fn frozen_normalization_test_root(path: &Path) -> fs::File {
    openat2_frozen(
        AT_FDCWD,
        path,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
    )
    .unwrap()
}

fn frozen_normalization_test_tree(
    entries: impl IntoIterator<Item = (PathBuf, FrozenExpectedEntry)>,
    limits: FrozenNormalizationLimits,
) -> Result<FrozenExpectedTree, Error> {
    let mut expected = BTreeMap::from([(
        PathBuf::from("/"),
        FrozenExpectedEntry {
            kind: FrozenExpectedKind::Directory,
            mode: 0o755,
        },
    )]);
    for (path, entry) in entries {
        assert!(expected.insert(path, entry).is_none());
    }
    FrozenExpectedTree::from_entries(expected, limits)
}

fn frozen_expected_directory(mode: u32) -> FrozenExpectedEntry {
    FrozenExpectedEntry {
        kind: FrozenExpectedKind::Directory,
        mode,
    }
}

fn frozen_expected_regular(mode: u32) -> FrozenExpectedEntry {
    FrozenExpectedEntry {
        kind: FrozenExpectedKind::Regular { digest: 0 },
        mode,
    }
}

fn frozen_expected_regular_bytes(mode: u32, bytes: &[u8]) -> FrozenExpectedEntry {
    FrozenExpectedEntry {
        kind: FrozenExpectedKind::Regular {
            digest: xxhash_rust::xxh3::xxh3_128(bytes),
        },
        mode,
    }
}

fn frozen_expected_symlink(mode: u32, target: &[u8]) -> FrozenExpectedEntry {
    FrozenExpectedEntry {
        kind: FrozenExpectedKind::Symlink {
            target: target.to_vec(),
        },
        mode,
    }
}

fn install_test_posix_acl(path: &Path, name: &CStr) -> bool {
    const ACL_UNDEFINED_ID: u32 = u32::MAX;
    // One named-user entry makes the ACL non-minimal so the kernel cannot
    // collapse it into ordinary mode bits.
    // SAFETY: geteuid takes no arguments and cannot fail.
    let named_user = unsafe { nix::libc::geteuid() };
    let entries = [
        (0x01_u16, 0o7_u16, ACL_UNDEFINED_ID),
        (0x02, 0o4, named_user),
        (0x04, 0o5, ACL_UNDEFINED_ID),
        (0x10, 0o5, ACL_UNDEFINED_ID),
        (0x20, 0o5, ACL_UNDEFINED_ID),
    ];
    let mut value = Vec::with_capacity(4 + entries.len() * 8);
    value.extend_from_slice(&2_u32.to_le_bytes());
    for (tag, permissions, id) in entries {
        value.extend_from_slice(&tag.to_le_bytes());
        value.extend_from_slice(&permissions.to_le_bytes());
        value.extend_from_slice(&id.to_le_bytes());
    }
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: both C strings and the complete xattr value remain live for
    // the call. The fixtures are private same-owner regular directories.
    if unsafe { nix::libc::setxattr(path.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0) } == 0 {
        return true;
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(nix::libc::EOPNOTSUPP) | Some(nix::libc::EPERM)
    ) {
        if std::env::var_os("CAST_REQUIRE_POSIX_ACL_TESTS").is_some() {
            panic!(
                "required POSIX ACL fixture is unavailable for {}: {error}",
                path.to_string_lossy()
            );
        }
        eprintln!("skipping POSIX ACL assertion for {}: {error}", path.to_string_lossy());
        false
    } else {
        panic!("install test POSIX ACL: {error}");
    }
}

#[test]
fn frozen_normalization_handles_mode_zero_entries_and_never_follows_symlinks() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    fs::create_dir(root_path.join("locked")).unwrap();
    fs::write(root_path.join("locked/file"), b"mode zero").unwrap();
    fs::set_permissions(root_path.join("locked/file"), Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(root_path.join("locked"), Permissions::from_mode(0o000)).unwrap();
    fs::write(&outside, b"external sentinel").unwrap();
    filetime::set_file_times(
        &outside,
        FileTime::from_unix_time(444, 0),
        FileTime::from_unix_time(444, 0),
    )
    .unwrap();
    symlink(&outside, root_path.join("link")).unwrap();
    let target = outside.as_os_str().as_bytes();
    let expected = frozen_normalization_test_tree(
        [
            (PathBuf::from("/link"), frozen_expected_symlink(0o777, target)),
            (PathBuf::from("/locked"), frozen_expected_directory(0o000)),
            (
                PathBuf::from("/locked/file"),
                frozen_expected_regular_bytes(0o000, b"mode zero"),
            ),
        ],
        FrozenNormalizationLimits { inodes: 4, depth: 2 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&root_path);
    let timestamp = FileTime::from_unix_time(123, 456);

    normalize_frozen_tree_with(
        &root,
        &root_path,
        &expected,
        timestamp,
        Instant::now() + Duration::from_secs(10),
        FrozenNormalizationLimits { inodes: 4, depth: 2 },
        |_, _| {},
    )
    .unwrap();

    for path in [root_path.clone(), root_path.join("locked"), root_path.join("link")] {
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert_eq!((metadata.atime(), metadata.atime_nsec()), (123, 456), "{path:?}");
        assert_eq!((metadata.mtime(), metadata.mtime_nsec()), (123, 456), "{path:?}");
    }
    assert_eq!(
        fs::symlink_metadata(root_path.join("locked")).unwrap().mode() & 0o7777,
        0
    );
    fs::set_permissions(root_path.join("locked"), Permissions::from_mode(0o700)).unwrap();
    let file_metadata = fs::symlink_metadata(root_path.join("locked/file")).unwrap();
    assert_eq!(file_metadata.mode() & 0o7777, 0);
    assert_eq!((file_metadata.atime(), file_metadata.atime_nsec()), (123, 456));
    assert_eq!((file_metadata.mtime(), file_metadata.mtime_nsec()), (123, 456));
    let outside_metadata = fs::symlink_metadata(&outside).unwrap();
    assert_eq!((outside_metadata.atime(), outside_metadata.mtime()), (444, 444));
    assert_eq!(fs::read(&outside).unwrap(), b"external sentinel");
    fs::set_permissions(root_path.join("locked/file"), Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn frozen_normalization_rejects_unplanned_missing_and_extra_entries() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    let unexpected = root_path.join("unexpected");
    fs::write(&unexpected, b"must not be normalized").unwrap();
    filetime::set_file_times(
        &unexpected,
        FileTime::from_unix_time(333, 0),
        FileTime::from_unix_time(333, 0),
    )
    .unwrap();
    let expected = frozen_normalization_test_tree([], FrozenNormalizationLimits { inodes: 2, depth: 1 }).unwrap();
    let root = frozen_normalization_test_root(&root_path);

    assert!(matches!(
        normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |_, _| {},
        ),
        Err(Error::FrozenNormalizationInventoryMismatch {
            reason: "the filesystem contains an undeclared entry",
            ..
        })
    ));
    assert_eq!(fs::symlink_metadata(&unexpected).unwrap().mtime(), 333);

    fs::remove_file(&unexpected).unwrap();
    let expected = frozen_normalization_test_tree(
        [(PathBuf::from("/missing"), frozen_expected_regular(0o600))],
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
    )
    .unwrap();
    assert!(matches!(
        normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |_, _| {},
        ),
        Err(Error::FrozenNormalizationInventoryMismatch {
            reason: "the filesystem is missing a declared entry",
            ..
        })
    ));
}

#[test]
fn frozen_normalization_directory_to_symlink_race_cannot_escape_root() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    fs::create_dir(root_path.join("child")).unwrap();
    fs::set_permissions(root_path.join("child"), Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, b"outside").unwrap();
    fs::set_permissions(&outside, Permissions::from_mode(0o500)).unwrap();
    filetime::set_file_times(
        &sentinel,
        FileTime::from_unix_time(777, 0),
        FileTime::from_unix_time(777, 0),
    )
    .unwrap();
    let outside_before = fs::symlink_metadata(&outside).unwrap();
    let expected = frozen_normalization_test_tree(
        [(PathBuf::from("/child"), frozen_expected_directory(0o700))],
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&root_path);
    let displaced = root_path.join("displaced");
    let mut raced = false;

    let error = normalize_frozen_tree_with(
        &root,
        &root_path,
        &expected,
        FileTime::from_unix_time(123, 0),
        Instant::now() + Duration::from_secs(10),
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
        |checkpoint, path| {
            if checkpoint == FrozenNormalizationCheckpoint::EntryPinned && path == Path::new("/child") && !raced {
                fs::rename(root_path.join("child"), &displaced).unwrap();
                symlink(&outside, root_path.join("child")).unwrap();
                raced = true;
            }
        },
    )
    .unwrap_err();
    assert!(raced);
    assert!(matches!(
        error,
        Error::FrozenNormalizationEntryChanged(_) | Error::OpenFrozenNormalizationEntry { .. }
    ));
    let outside_after = fs::symlink_metadata(&outside).unwrap();
    assert_eq!(outside_after.mode(), outside_before.mode());
    assert_eq!(
        (outside_after.atime(), outside_after.mtime()),
        (outside_before.atime(), outside_before.mtime())
    );
    assert_eq!(fs::symlink_metadata(&sentinel).unwrap().mtime(), 777);
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside");
    fs::set_permissions(&outside, Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn frozen_normalization_hardlink_substitution_is_rejected_before_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    fs::write(root_path.join("file"), b"declared").unwrap();
    fs::set_permissions(root_path.join("file"), Permissions::from_mode(0o600)).unwrap();
    fs::write(&outside, b"external sentinel").unwrap();
    fs::set_permissions(&outside, Permissions::from_mode(0o640)).unwrap();
    filetime::set_file_times(
        &outside,
        FileTime::from_unix_time(888, 0),
        FileTime::from_unix_time(888, 0),
    )
    .unwrap();
    let expected = frozen_normalization_test_tree(
        [(PathBuf::from("/file"), frozen_expected_regular(0o600))],
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&root_path);
    let displaced = root_path.join("displaced");
    let mut raced = false;

    let error = normalize_frozen_tree_with(
        &root,
        &root_path,
        &expected,
        FileTime::from_unix_time(123, 0),
        Instant::now() + Duration::from_secs(10),
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
        |checkpoint, path| {
            if checkpoint == FrozenNormalizationCheckpoint::EntryPinned && path == Path::new("/file") && !raced {
                fs::rename(root_path.join("file"), &displaced).unwrap();
                fs::hard_link(&outside, root_path.join("file")).unwrap();
                raced = true;
            }
        },
    )
    .unwrap_err();
    assert!(raced);
    assert!(matches!(error, Error::FrozenNormalizationEntryChanged(_)));
    let outside_metadata = fs::symlink_metadata(&outside).unwrap();
    assert_eq!(outside_metadata.mode() & 0o7777, 0o640);
    assert_eq!((outside_metadata.atime(), outside_metadata.mtime()), (888, 888));
    assert_eq!(fs::read(&outside).unwrap(), b"external sentinel");
}

#[test]
fn frozen_normalization_rejects_stage_root_name_substitution() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    let displaced = temporary.path().join("displaced");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    let expected = frozen_normalization_test_tree([], FrozenNormalizationLimits { inodes: 1, depth: 0 }).unwrap();
    let root = frozen_normalization_test_root(&root_path);
    let mut raced = false;

    let error = normalize_frozen_tree_with(
        &root,
        &root_path,
        &expected,
        FileTime::from_unix_time(123, 0),
        Instant::now() + Duration::from_secs(10),
        FrozenNormalizationLimits { inodes: 1, depth: 0 },
        |checkpoint, path| {
            if checkpoint == FrozenNormalizationCheckpoint::BeforeRootRevalidation && path == Path::new("/") {
                fs::rename(&root_path, &displaced).unwrap();
                fs::create_dir(&root_path).unwrap();
                fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
                fs::write(root_path.join("replacement"), b"must not publish").unwrap();
                raced = true;
            }
        },
    )
    .unwrap_err();
    assert!(raced);
    assert!(match error {
        Error::FrozenNormalizationRootChanged(path) => path == root_path,
        Error::FrozenNormalizationEntryChanged(path) => path == Path::new("/"),
        _ => false,
    });
    assert_eq!(fs::read(root_path.join("replacement")).unwrap(), b"must not publish");
    assert_eq!(fs::symlink_metadata(&displaced).unwrap().mtime(), 123);
}

#[test]
fn frozen_normalization_limits_accept_n_and_reject_n_plus_one() {
    let entries = || {
        [
            (PathBuf::from("/a"), frozen_expected_directory(0o755)),
            (PathBuf::from("/a/b"), frozen_expected_regular(0o600)),
        ]
    };
    assert!(frozen_normalization_test_tree(entries(), FrozenNormalizationLimits { inodes: 3, depth: 2 }).is_ok());
    assert!(matches!(
        frozen_normalization_test_tree(entries(), FrozenNormalizationLimits { inodes: 2, depth: 2 }),
        Err(Error::FrozenNormalizationInodeLimit { limit: 2, actual: 3 })
    ));
    assert!(matches!(
        frozen_normalization_test_tree(entries(), FrozenNormalizationLimits { inodes: 3, depth: 1 }),
        Err(Error::FrozenNormalizationDepthLimit { limit: 1, actual: 2 })
    ));
}

#[test]
fn frozen_normalization_runtime_walk_enforces_the_inode_limit() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    fs::create_dir_all(root_path.join("nested")).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(root_path.join("nested"), Permissions::from_mode(0o755)).unwrap();
    fs::write(root_path.join("nested/file"), b"bounded").unwrap();
    fs::set_permissions(root_path.join("nested/file"), Permissions::from_mode(0o600)).unwrap();
    let expected = frozen_normalization_test_tree(
        [
            (PathBuf::from("/nested"), frozen_expected_directory(0o755)),
            (
                PathBuf::from("/nested/file"),
                frozen_expected_regular_bytes(0o600, b"bounded"),
            ),
        ],
        FrozenNormalizationLimits { inodes: 3, depth: 2 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&root_path);

    assert!(matches!(
        normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 2 },
            |_, _| {},
        ),
        Err(Error::FrozenNormalizationInodeLimit { limit: 2, actual: 3 })
    ));
}

#[test]
fn frozen_normalization_rejects_regular_content_outside_the_declaration() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    let file = root_path.join("file");
    fs::write(&file, b"tampered").unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
    filetime::set_file_times(
        &file,
        FileTime::from_unix_time(999, 0),
        FileTime::from_unix_time(999, 0),
    )
    .unwrap();
    let expected = frozen_normalization_test_tree(
        [(
            PathBuf::from("/file"),
            frozen_expected_regular_bytes(0o600, b"declared"),
        )],
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&root_path);

    assert!(matches!(
        normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |_, _| {},
        ),
        Err(Error::FrozenNormalizationInventoryMismatch {
            reason: "the regular file content digest differs from its declaration",
            ..
        })
    ));
    assert_eq!(fs::symlink_metadata(&file).unwrap().mode() & 0o7777, 0o600);
    assert_eq!(fs::read(&file).unwrap(), b"tampered");
}

#[test]
fn frozen_normalization_detects_same_inode_mutation_before_final_revalidation() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    let file = root_path.join("file");
    fs::write(&file, b"original").unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
    let expected = frozen_normalization_test_tree(
        [(
            PathBuf::from("/file"),
            frozen_expected_regular_bytes(0o600, b"original"),
        )],
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&root_path);
    let mut raced = false;

    let error = normalize_frozen_tree_with(
        &root,
        &root_path,
        &expected,
        FileTime::from_unix_time(123, 0),
        Instant::now() + Duration::from_secs(10),
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
        |checkpoint, path| {
            if checkpoint == FrozenNormalizationCheckpoint::AfterRegularDigest && path == Path::new("/file") && !raced {
                fs::write(&file, b"mutated!").unwrap();
                raced = true;
            }
        },
    )
    .unwrap_err();
    assert!(raced);
    assert!(matches!(error, Error::FrozenNormalizationEntryChanged(path) if path == Path::new("/file")));
    assert_eq!(fs::read(&file).unwrap(), b"mutated!");
}

#[test]
fn frozen_normalization_final_pass_detects_deep_content_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    fs::create_dir_all(root_path.join("nested")).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(root_path.join("nested"), Permissions::from_mode(0o755)).unwrap();
    let file = root_path.join("nested/file");
    fs::write(&file, b"original").unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
    let expected = frozen_normalization_test_tree(
        [
            (PathBuf::from("/nested"), frozen_expected_directory(0o755)),
            (
                PathBuf::from("/nested/file"),
                frozen_expected_regular_bytes(0o600, b"original"),
            ),
        ],
        FrozenNormalizationLimits { inodes: 3, depth: 2 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&root_path);
    let mut raced = false;

    let error = normalize_frozen_tree_with(
        &root,
        &root_path,
        &expected,
        FileTime::from_unix_time(123, 0),
        Instant::now() + Duration::from_secs(10),
        FrozenNormalizationLimits { inodes: 3, depth: 2 },
        |checkpoint, path| {
            if checkpoint == FrozenNormalizationCheckpoint::BeforeFinalTreeConfirmation && path == Path::new("/") {
                fs::write(&file, b"mutated!").unwrap();
                raced = true;
            }
        },
    )
    .unwrap_err();
    assert!(raced);
    assert!(matches!(
        error,
        Error::FrozenNormalizationInventoryMismatch {
            reason: "the regular file content digest differs from its declaration",
            ..
        }
    ));
}

#[test]
fn frozen_normalization_root_inventory_detects_post_digest_child_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    let file = root_path.join("file");
    fs::write(&file, b"original").unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
    let expected = frozen_normalization_test_tree(
        [(
            PathBuf::from("/file"),
            frozen_expected_regular_bytes(0o600, b"original"),
        )],
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&root_path);
    let mut raced = false;

    let error = normalize_frozen_tree_with(
        &root,
        &root_path,
        &expected,
        FileTime::from_unix_time(123, 0),
        Instant::now() + Duration::from_secs(10),
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
        |checkpoint, path| {
            if checkpoint == FrozenNormalizationCheckpoint::BeforeRootRevalidation && path == Path::new("/") {
                fs::write(&file, b"mutated!").unwrap();
                raced = true;
            }
        },
    )
    .unwrap_err();
    assert!(raced);
    assert!(matches!(error, Error::FrozenNormalizationEntryChanged(path) if path == Path::new("/file")));
}

#[test]
fn frozen_normalization_detects_entry_added_after_final_inventory() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    let expected = frozen_normalization_test_tree([], FrozenNormalizationLimits { inodes: 1, depth: 0 }).unwrap();
    let root = frozen_normalization_test_root(&root_path);
    let mut raced = false;

    let error = normalize_frozen_tree_with(
        &root,
        &root_path,
        &expected,
        FileTime::from_unix_time(123, 0),
        Instant::now() + Duration::from_secs(10),
        FrozenNormalizationLimits { inodes: 1, depth: 0 },
        |checkpoint, path| {
            if checkpoint == FrozenNormalizationCheckpoint::AfterDirectoryFinalInventory && path == Path::new("/") {
                fs::write(root_path.join("late"), b"must not publish").unwrap();
                raced = true;
            }
        },
    )
    .unwrap_err();
    assert!(raced);
    assert!(matches!(error, Error::FrozenNormalizationEntryChanged(path) if path == Path::new("/")));
    assert_eq!(fs::read(root_path.join("late")).unwrap(), b"must not publish");
}

#[test]
fn frozen_normalization_orders_non_utf8_names_as_raw_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("root");
    fs::create_dir(&root_path).unwrap();
    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
    let name = OsString::from_vec(vec![b'n', 0xff]);
    let file = root_path.join(&name);
    fs::write(&file, b"raw name").unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
    let expected_path = Path::new("/").join(&name);
    let expected = frozen_normalization_test_tree(
        [(expected_path, frozen_expected_regular_bytes(0o600, b"raw name"))],
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&root_path);

    normalize_frozen_tree_with(
        &root,
        &root_path,
        &expected,
        FileTime::from_unix_time(123, 0),
        Instant::now() + Duration::from_secs(10),
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
        |_, _| {},
    )
    .unwrap();

    assert_eq!(fs::symlink_metadata(&file).unwrap().mtime(), 123);
}

#[test]
fn frozen_normalization_rejects_cross_mount_entries_before_mutation() {
    let root = fs::File::open("/").unwrap();
    for name in [c"proc", c"sys", c"dev"] {
        match open_frozen_normalization_entry(
            &root,
            name,
            Path::new("/").join(OsStr::from_bytes(name.to_bytes())).as_path(),
            FrozenNormalizationOpen::Anchor,
            Instant::now() + Duration::from_secs(10),
        ) {
            Err(Error::OpenFrozenNormalizationEntry { source, .. })
                if source.raw_os_error() == Some(nix::libc::EXDEV) =>
            {
                return;
            }
            Ok(_) => {}
            Err(error) => panic!("unexpected cross-mount probe failure: {error}"),
        }
    }
    panic!("expected /proc, /sys, or /dev to reside on another mount");
}

#[test]
fn frozen_normalization_rejects_access_acl_after_active_mode_change() {
    let access = tempfile::tempdir().unwrap();
    let access_root = access.path().join("root");
    fs::create_dir(&access_root).unwrap();
    fs::set_permissions(&access_root, Permissions::from_mode(0o755)).unwrap();
    let file = access_root.join("file");
    fs::write(&file, b"acl protected").unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o640)).unwrap();
    if !install_test_posix_acl(&file, c"system.posix_acl_access") {
        return;
    }
    // Preserve the non-minimal ACL while forcing phase one to add owner
    // read permission through the retained descriptor.
    fs::set_permissions(&file, Permissions::from_mode(0o000)).unwrap();
    let file_mode = fs::symlink_metadata(&file).unwrap().mode() & 0o7777;
    assert_eq!(file_mode, 0);
    let expected = frozen_normalization_test_tree(
        [(
            PathBuf::from("/file"),
            frozen_expected_regular_bytes(file_mode, b"acl protected"),
        )],
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&access_root);
    assert!(matches!(
        normalize_frozen_tree_with(
            &root,
            &access_root,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |_, _| {},
        ),
        Err(Error::FrozenNormalizationAcl { path, .. }) if path == Path::new("/file")
    ));
}

#[test]
fn frozen_normalization_rejects_default_acl_after_active_mode_change() {
    let default = tempfile::tempdir().unwrap();
    let default_root = default.path().join("root");
    let directory = default_root.join("directory");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&default_root, Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&directory, Permissions::from_mode(0o755)).unwrap();
    if !install_test_posix_acl(&directory, c"system.posix_acl_default") {
        return;
    }
    // Force phase one to add traversal permission; a default ACL must
    // remain visible and rejected after that descriptor mutation.
    fs::set_permissions(&directory, Permissions::from_mode(0o000)).unwrap();
    let directory_mode = fs::symlink_metadata(&directory).unwrap().mode() & 0o7777;
    assert_eq!(directory_mode, 0);
    let expected = frozen_normalization_test_tree(
        [(PathBuf::from("/directory"), frozen_expected_directory(directory_mode))],
        FrozenNormalizationLimits { inodes: 2, depth: 1 },
    )
    .unwrap();
    let root = frozen_normalization_test_root(&default_root);
    assert!(matches!(
        normalize_frozen_tree_with(
            &root,
            &default_root,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |_, _| {},
        ),
        Err(Error::FrozenNormalizationAcl { path, .. }) if path == Path::new("/directory")
    ));
}
