use std::os::unix::ffi::OsStringExt as _;

#[test]
fn anchored_exact_locator_is_owned_and_reopens_only_the_same_identity() {
    let root = tempfile::tempdir().unwrap();
    let expected = fs::metadata(root.path()).unwrap();
    let witness = open_path_directory(root.path());
    let locator = AnchoredLocator::exact(root.path(), &witness).unwrap();
    assert_eq!(locator.absolute_base_path(), root.path());
    assert_eq!(locator.relative_path(), None);

    drop(witness);
    let namespace_root = open_path_directory(Path::new("/"));
    let reopened = locator.reopen_from_namespace_root(namespace_root.as_raw_fd()).unwrap();
    let actual = descriptor_stat(reopened.as_raw_fd()).unwrap();
    assert_eq!(actual.st_dev as u64, expected.dev());
    assert_eq!(actual.st_ino as u64, expected.ino());
    assert_eq!(actual.st_mode & nix::libc::S_IFMT, nix::libc::S_IFDIR);
}

#[test]
fn anchored_locator_path_bounds_admit_n_and_reject_n_plus_one() {
    let root = open_path_directory(Path::new("/"));
    let temporary = tempfile::tempdir().unwrap();
    let leaf_path = temporary.path().join("leaf");
    fs::write(&leaf_path, b"leaf").unwrap();
    let base = open_path_directory(temporary.path());
    let leaf = open_path_file(&leaf_path);

    let mut absolute_components = std::iter::repeat_n("a".repeat(255), 15).collect::<Vec<_>>();
    absolute_components.push("b".repeat(254));
    let maximal_absolute = format!("/{}", absolute_components.join("/"));
    assert_eq!(maximal_absolute.len(), 4095);
    assert!(!matches!(
        AnchoredLocator::exact(&maximal_absolute, &root),
        Err(AnchoredLocatorError::InvalidAbsolute { .. })
    ));
    assert!(matches!(
        AnchoredLocator::exact(format!("{maximal_absolute}x"), &root),
        Err(AnchoredLocatorError::InvalidAbsolute { .. })
    ));

    let maximal_relative = std::iter::repeat_n("a".repeat(255), 16).collect::<Vec<_>>().join("/");
    assert_eq!(maximal_relative.len(), 4095);
    assert!(!matches!(
        AnchoredLocator::beneath(temporary.path(), &base, &maximal_relative, &leaf),
        Err(AnchoredLocatorError::InvalidRelative { .. })
    ));
    assert!(matches!(
        AnchoredLocator::beneath(temporary.path(), &base, format!("{maximal_relative}x"), &leaf),
        Err(AnchoredLocatorError::InvalidRelative { .. })
    ));

    let component_n = "a".repeat(255);
    let component_n_plus_one = "a".repeat(256);
    assert!(!matches!(
        AnchoredLocator::exact(format!("/{component_n}"), &root),
        Err(AnchoredLocatorError::InvalidAbsolute { .. })
    ));
    assert!(matches!(
        AnchoredLocator::exact(format!("/{component_n_plus_one}"), &root),
        Err(AnchoredLocatorError::InvalidAbsolute { .. })
    ));

    let components_n = std::iter::repeat_n("a", 256).collect::<Vec<_>>().join("/");
    let components_n_plus_one = std::iter::repeat_n("a", 257).collect::<Vec<_>>().join("/");
    assert!(!matches!(
        AnchoredLocator::beneath(temporary.path(), &base, &components_n, &leaf),
        Err(AnchoredLocatorError::InvalidRelative { .. })
    ));
    assert!(matches!(
        AnchoredLocator::beneath(temporary.path(), &base, &components_n_plus_one, &leaf),
        Err(AnchoredLocatorError::InvalidRelative { .. })
    ));

    let absolute_nul = PathBuf::from(std::ffi::OsString::from_vec(b"/safe\0leaf".to_vec()));
    let relative_nul = PathBuf::from(std::ffi::OsString::from_vec(b"safe\0leaf".to_vec()));
    assert!(matches!(
        AnchoredLocator::exact(absolute_nul, &root),
        Err(AnchoredLocatorError::InvalidAbsolute { .. })
    ));
    assert!(matches!(
        AnchoredLocator::beneath(temporary.path(), &base, relative_nul, &leaf),
        Err(AnchoredLocatorError::InvalidRelative { .. })
    ));
}

#[test]
fn anchored_locator_accepts_exact_root_rejects_special_witnesses_and_retains_cloexec_ownership() {
    let root = open_path_directory(Path::new("/"));
    let caller_root = root.as_raw_fd();
    let exact = AnchoredLocator::exact("/", &root).unwrap();
    let (retained_root, no_leaf) = exact.retained_descriptors();
    assert_ne!(retained_root, caller_root);
    assert_eq!(no_leaf, None);
    let flags = FdFlag::from_bits_truncate(fcntl(retained_root, FcntlArg::F_GETFD).unwrap());
    assert!(flags.contains(FdFlag::FD_CLOEXEC));
    drop(root);
    assert!(fcntl(retained_root, FcntlArg::F_GETFD).is_ok());
    let namespace_root = open_path_directory(Path::new("/"));
    exact.reopen_from_namespace_root(namespace_root.as_raw_fd()).unwrap();

    let temporary = tempfile::tempdir().unwrap();
    let leaf_path = temporary.path().join("leaf");
    fs::write(&leaf_path, b"leaf").unwrap();
    let base = open_path_directory(temporary.path());
    let leaf = open_path_file(&leaf_path);
    let base_caller = base.as_raw_fd();
    let leaf_caller = leaf.as_raw_fd();
    let beneath = AnchoredLocator::beneath(temporary.path(), &base, "leaf", &leaf).unwrap();
    let (retained_base, retained_leaf) = beneath.retained_descriptors();
    let retained_leaf = retained_leaf.unwrap();
    assert_ne!(retained_base, base_caller);
    assert_ne!(retained_leaf, leaf_caller);
    for descriptor in [retained_base, retained_leaf] {
        let flags = FdFlag::from_bits_truncate(fcntl(descriptor, FcntlArg::F_GETFD).unwrap());
        assert!(flags.contains(FdFlag::FD_CLOEXEC));
    }
    drop(base);
    drop(leaf);
    beneath.reopen_from_namespace_root(namespace_root.as_raw_fd()).unwrap();

    let fifo_path = temporary.path().join("fifo");
    mkfifo(&fifo_path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let fifo = open_path_file(&fifo_path);
    assert!(matches!(
        AnchoredLocator::exact(&fifo_path, &fifo),
        Err(AnchoredLocatorError::UnsupportedWitnessType {
            component: AnchoredLocatorComponent::Exact,
            file_type: nix::libc::S_IFIFO,
            ..
        })
    ));
    let device = open_path_file(Path::new("/dev/null"));
    assert!(matches!(
        AnchoredLocator::exact("/dev/null", &device),
        Err(AnchoredLocatorError::UnsupportedWitnessType {
            component: AnchoredLocatorComponent::Exact,
            file_type: nix::libc::S_IFCHR,
            ..
        })
    ));
}

#[test]
fn anchored_locator_rejects_relative_aliased_and_unsafe_syntax() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("child");
    fs::write(&child, b"leaf").unwrap();
    let base = open_path_directory(root.path());
    let leaf = open_path_file(&child);

    for invalid in [
        "",
        ".",
        "relative",
        "../escape",
        "/safe/../escape",
        "/safe/./leaf",
        "//safe",
        "/safe//leaf",
        "/safe/",
    ] {
        let error = AnchoredLocator::exact(invalid, &base).unwrap_err();
        assert!(
            matches!(error, AnchoredLocatorError::InvalidAbsolute { .. }),
            "accepted {invalid:?}"
        );
    }
    for invalid in [
        "",
        ".",
        "..",
        "/absolute",
        "safe/../escape",
        "safe/./leaf",
        "safe//leaf",
        "safe/",
    ] {
        let error = AnchoredLocator::beneath(root.path(), &base, invalid, &leaf).unwrap_err();
        assert!(
            matches!(error, AnchoredLocatorError::InvalidRelative { .. }),
            "accepted {invalid:?}"
        );
    }

    let ordinary = fs::File::open(&child).unwrap();
    assert!(matches!(
        AnchoredLocator::exact(&child, &ordinary),
        Err(AnchoredLocatorError::WitnessNotPath {
            component: AnchoredLocatorComponent::Exact,
            ..
        })
    ));
    assert!(matches!(
        AnchoredLocator::beneath(&child, &leaf, "leaf", &leaf),
        Err(AnchoredLocatorError::BeneathBaseNotDirectory { .. })
    ));
}

#[test]
fn anchored_locator_construction_rejects_initial_identity_mismatch_symlink_and_missing_path() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(first.join("leaf"), b"first").unwrap();
    fs::write(first.join("other"), b"other").unwrap();
    let first_base = open_path_directory(&first);
    let second_base = open_path_directory(&second);
    let leaf = open_path_file(&first.join("leaf"));
    let other = open_path_file(&first.join("other"));

    assert!(matches!(
        AnchoredLocator::exact(&first, &second_base),
        Err(AnchoredLocatorError::IdentityMismatch {
            component: AnchoredLocatorComponent::Exact,
            ..
        })
    ));
    assert!(matches!(
        AnchoredLocator::beneath(&first, &second_base, "leaf", &leaf),
        Err(AnchoredLocatorError::IdentityMismatch {
            component: AnchoredLocatorComponent::BeneathBase,
            ..
        })
    ));
    assert!(matches!(
        AnchoredLocator::beneath(&first, &first_base, "leaf", &other),
        Err(AnchoredLocatorError::IdentityMismatch {
            component: AnchoredLocatorComponent::BeneathLeaf,
            ..
        })
    ));

    std::os::unix::fs::symlink(first.join("leaf"), temporary.path().join("redirect")).unwrap();
    assert!(matches!(
        AnchoredLocator::exact(temporary.path().join("redirect"), &leaf),
        Err(AnchoredLocatorError::Reopen {
            component: AnchoredLocatorComponent::Exact,
            source,
            ..
        }) if source.raw_os_error() == Some(nix::libc::ELOOP)
    ));
    assert!(matches!(
        AnchoredLocator::beneath(&first, &first_base, "missing", &leaf),
        Err(AnchoredLocatorError::Reopen {
            component: AnchoredLocatorComponent::BeneathLeaf,
            source,
            ..
        }) if source.raw_os_error() == Some(nix::libc::ENOENT)
    ));
}

#[test]
fn anchored_exact_locator_allows_mount_crossing_but_rejects_symlinks() {
    let namespace_root = open_path_directory(Path::new("/"));
    let proc_root = open_path_directory(Path::new("/proc"));
    let proc_locator = AnchoredLocator::exact("/proc", &proc_root).unwrap();
    let reopened = proc_locator
        .reopen_from_namespace_root(namespace_root.as_raw_fd())
        .unwrap();
    let expected = descriptor_stat(proc_root.as_raw_fd()).unwrap();
    let actual = descriptor_stat(reopened.as_raw_fd()).unwrap();
    assert_eq!(actual.st_dev, expected.st_dev);
    assert_eq!(actual.st_ino, expected.st_ino);

    let proc_self = open_path_directory(Path::new("/proc/self"));
    assert!(matches!(
        AnchoredLocator::exact("/proc/self", &proc_self),
        Err(AnchoredLocatorError::Reopen {
            component: AnchoredLocatorComponent::Exact,
            source,
            ..
        }) if source.raw_os_error() == Some(nix::libc::ELOOP)
    ));
}

#[test]
fn anchored_exact_locator_rejects_replacement_but_accepts_same_inode_hardlink() {
    let temporary = tempfile::tempdir().unwrap();
    let namespace_root = open_path_directory(Path::new("/"));
    let original = temporary.path().join("original");
    let retained = temporary.path().join("retained");
    fs::create_dir(&original).unwrap();
    let witness = open_path_directory(&original);
    let locator = AnchoredLocator::exact(&original, &witness).unwrap();
    fs::rename(&original, &retained).unwrap();
    fs::create_dir(&original).unwrap();
    assert!(matches!(
        locator.reopen_from_namespace_root(namespace_root.as_raw_fd()),
        Err(AnchoredLocatorError::IdentityMismatch {
            component: AnchoredLocatorComponent::Exact,
            ..
        })
    ));

    let file = temporary.path().join("file");
    let alias = temporary.path().join("alias");
    fs::write(&file, b"same inode").unwrap();
    fs::hard_link(&file, &alias).unwrap();
    let file_witness = open_path_file(&file);
    let hardlink_locator = AnchoredLocator::exact(&file, &file_witness).unwrap();
    fs::remove_file(&file).unwrap();
    fs::hard_link(&alias, &file).unwrap();
    hardlink_locator
        .reopen_from_namespace_root(namespace_root.as_raw_fd())
        .unwrap();
}

#[test]
fn anchored_beneath_locator_authenticates_base_and_leaf_independently() {
    let temporary = tempfile::tempdir().unwrap();
    let namespace_root = open_path_directory(Path::new("/"));
    let base_path = temporary.path().join("base");
    let retained_base = temporary.path().join("retained-base");
    fs::create_dir(&base_path).unwrap();
    fs::write(base_path.join("leaf"), b"expected").unwrap();
    let base = open_path_directory(&base_path);
    let leaf = open_path_file(&base_path.join("leaf"));
    let locator = AnchoredLocator::beneath(&base_path, &base, "leaf", &leaf).unwrap();
    assert_eq!(locator.absolute_base_path(), base_path);
    assert_eq!(locator.relative_path(), Some(Path::new("leaf")));
    locator.reopen_from_namespace_root(namespace_root.as_raw_fd()).unwrap();

    fs::rename(&base_path, &retained_base).unwrap();
    fs::create_dir(&base_path).unwrap();
    fs::write(base_path.join("leaf"), b"replacement").unwrap();
    assert!(matches!(
        locator.reopen_from_namespace_root(namespace_root.as_raw_fd()),
        Err(AnchoredLocatorError::IdentityMismatch {
            component: AnchoredLocatorComponent::BeneathBase,
            ..
        })
    ));
}

#[test]
fn anchored_beneath_locator_rejects_leaf_replacement_symlinks_and_mount_crossing() {
    let temporary = tempfile::tempdir().unwrap();
    let namespace_root = open_path_directory(Path::new("/"));
    let leaf_path = temporary.path().join("leaf");
    let retained_leaf = temporary.path().join("retained-leaf");
    fs::write(&leaf_path, b"expected").unwrap();
    let base = open_path_directory(temporary.path());
    let leaf = open_path_file(&leaf_path);
    let locator = AnchoredLocator::beneath(temporary.path(), &base, "leaf", &leaf).unwrap();
    fs::rename(&leaf_path, &retained_leaf).unwrap();
    fs::create_dir(&leaf_path).unwrap();
    assert!(matches!(
        locator.reopen_from_namespace_root(namespace_root.as_raw_fd()),
        Err(AnchoredLocatorError::IdentityMismatch {
            component: AnchoredLocatorComponent::BeneathLeaf,
            actual_file_type: nix::libc::S_IFDIR,
            ..
        })
    ));

    let symlink_path = temporary.path().join("redirect");
    std::os::unix::fs::symlink(&retained_leaf, &symlink_path).unwrap();
    assert!(matches!(
        AnchoredLocator::beneath(temporary.path(), &base, "redirect", &leaf),
        Err(AnchoredLocatorError::Reopen {
            component: AnchoredLocatorComponent::BeneathLeaf,
            source,
            ..
        }) if source.raw_os_error() == Some(nix::libc::ELOOP)
    ));

    let proc_root = open_path_directory(Path::new("/proc"));
    assert!(matches!(
        AnchoredLocator::beneath("/", &namespace_root, "proc", &proc_root),
        Err(AnchoredLocatorError::Reopen {
            component: AnchoredLocatorComponent::BeneathLeaf,
            source,
            ..
        }) if source.raw_os_error() == Some(nix::libc::EXDEV)
    ));
}
