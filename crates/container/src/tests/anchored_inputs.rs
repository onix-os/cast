#[test]
fn anchored_constructor_owns_a_cloexec_opath_directory_duplicate() {
    let root = tempfile::tempdir().unwrap();
    let anchor = open_path_directory(root.path());
    let caller_descriptor = anchor.as_raw_fd();
    let container = Container::new_anchored("diagnostic-root", &anchor).unwrap();
    let retained = container.root_anchor.as_ref().unwrap().as_raw_fd();

    assert_ne!(retained, caller_descriptor);
    assert_eq!(container.root, Path::new("diagnostic-root"));
    let status = fcntl(retained, FcntlArg::F_GETFL).unwrap();
    assert_eq!(status & nix::libc::O_PATH, nix::libc::O_PATH);
    let descriptor = FdFlag::from_bits_truncate(fcntl(retained, FcntlArg::F_GETFD).unwrap());
    assert!(descriptor.contains(FdFlag::FD_CLOEXEC));

    drop(anchor);
    assert!(fcntl(retained, FcntlArg::F_GETFD).is_ok());
}

#[test]
fn anchored_constructor_rejects_every_non_opath_or_non_directory_descriptor() {
    let root = tempfile::tempdir().unwrap();
    let ordinary_directory = std::fs::File::open(root.path()).unwrap();
    let error = Container::new_anchored(root.path(), &ordinary_directory).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        error.to_string(),
        "anchored container root descriptor must be opened with O_PATH"
    );

    let regular_path = root.path().join("regular");
    fs::write(&regular_path, b"not a directory").unwrap();
    let ordinary_file = open_path_file(&regular_path);
    let error = Container::new_anchored(root.path(), &ordinary_file).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        error.to_string(),
        "anchored container root descriptor must reference a directory"
    );

    struct InvalidDescriptor;
    impl std::os::fd::AsRawFd for InvalidDescriptor {
        fn as_raw_fd(&self) -> std::os::fd::RawFd {
            -1
        }
    }
    let error = Container::new_anchored(root.path(), &InvalidDescriptor).err().unwrap();
    assert_eq!(error.raw_os_error(), Some(nix::libc::EBADF));
}

#[test]
fn anchored_bind_source_is_pinned_before_clone_and_survives_path_substitution() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    let pinned_name = temporary.path().join("pinned-source");
    fs::write(&source, b"authenticated source").unwrap();
    let expected = fs::metadata(&source).unwrap();

    let root_anchor = open_path_directory(temporary.path());
    let pinned = pin_anchored_bind_sources(
        root_anchor.as_raw_fd(),
        &[Bind {
            source: BindSource::RootRelative(PathBuf::from("source")),
            target: PathBuf::from("/payload/source"),
            read_only: true,
        }],
    )
    .unwrap();

    // This is the adversarial checkpoint: after the supervising process
    // has pinned the source but before the child clones its mount, replace
    // the complete pathname with a different object.
    fs::rename(&source, &pinned_name).unwrap();
    fs::write(&source, b"replacement source").unwrap();

    let retained = descriptor_stat(pinned[0].source.as_raw_fd()).unwrap();
    assert_eq!(retained.st_dev as u64, expected.dev());
    assert_eq!(retained.st_ino as u64, expected.ino());
    let mut retained_reader = reopen_pinned_readonly(pinned[0].source.as_raw_fd()).unwrap();
    let mut retained_bytes = Vec::new();
    retained_reader.read_to_end(&mut retained_bytes).unwrap();
    assert_eq!(retained_bytes, b"authenticated source");
    assert_eq!(fs::read(&source).unwrap(), b"replacement source");
    assert_eq!(pinned[0].target, Path::new("payload/source"));
}

#[test]
fn anchored_mount_targets_must_preexist_and_reject_symlink_traversal() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("target"), b"outside witness").unwrap();
    let anchor = open_path_directory(&root);

    let missing = open_anchored_mount_target(
        anchor.as_raw_fd(),
        Path::new("missing"),
        AnchoredMountTargetKind::RegularFile,
    )
    .unwrap_err();
    assert!(matches!(
        missing,
        ContainerError::OpenAnchoredMountTarget { source, .. }
            if source.raw_os_error() == Some(nix::libc::ENOENT)
    ));
    assert!(
        !root.join("missing").exists(),
        "target resolution must never create mountpoints"
    );

    std::os::unix::fs::symlink(&outside, root.join("redirect")).unwrap();
    let redirected = open_anchored_mount_target(
        anchor.as_raw_fd(),
        Path::new("redirect/target"),
        AnchoredMountTargetKind::RegularFile,
    )
    .unwrap_err();
    assert!(matches!(redirected, ContainerError::OpenAnchoredMountTarget { .. }));
    assert_eq!(fs::read(outside.join("target")).unwrap(), b"outside witness");

    let host_root = open_path_directory(Path::new("/"));
    let nested_mount = open_anchored_mount_target(
        host_root.as_raw_fd(),
        Path::new("proc/self"),
        AnchoredMountTargetKind::Directory,
    )
    .unwrap_err();
    assert!(matches!(
        nested_mount,
        ContainerError::OpenAnchoredMountTarget { source, .. }
            if source.raw_os_error() == Some(nix::libc::EXDEV)
    ));
}

#[test]
fn anchored_mount_target_normalization_rejects_escape_and_root_aliases() {
    for invalid in [
        "",
        "/",
        ".",
        "relative",
        "../escape",
        "/safe/../escape",
        "/safe/./target",
    ] {
        assert!(
            normalized_anchored_mount_target(Path::new(invalid)).is_err(),
            "accepted {invalid:?}"
        );
    }
    assert_eq!(
        normalized_anchored_mount_target(Path::new("/safe/target")).unwrap(),
        Path::new("safe/target")
    );

    let mut maximal_components = std::iter::repeat_n("a".repeat(255), 15).collect::<Vec<_>>();
    maximal_components.push("b".repeat(254));
    let maximal = format!("/{}", maximal_components.join("/"));
    assert_eq!(maximal.len(), 4095);
    assert!(normalized_anchored_mount_target(Path::new(&maximal)).is_ok());
    assert!(normalized_anchored_mount_target(Path::new(&format!("{maximal}x"))).is_err());
}

#[test]
fn anchored_mount_topology_rejects_duplicate_and_nested_targets() {
    let source = tempfile::tempdir().unwrap();
    let mounts = |first: &str, second: &str| {
        vec![
            PreparedAnchoredMount {
                source_mount: open_path_directory(source.path()),
                target: PathBuf::from(first),
                target_kind: AnchoredMountTargetKind::Directory,
            },
            PreparedAnchoredMount {
                source_mount: open_path_directory(source.path()),
                target: PathBuf::from(second),
                target_kind: AnchoredMountTargetKind::Directory,
            },
        ]
    };

    for (first, second) in [("work", "work"), ("work", "work/cache"), ("work/cache", "work")] {
        assert!(matches!(
            validate_anchored_mount_topology(&mounts(first, second)),
            Err(ContainerError::OverlappingAnchoredMountTargets { .. })
        ));
    }
    validate_anchored_mount_topology(&mounts("work", "cache")).unwrap();
}

#[test]
fn anchored_execution_rejects_pathname_and_special_file_bind_sources_before_clone() {
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let anchor = open_path_directory(root.path());
    let path_error = pin_anchored_bind_sources(
        anchor.as_raw_fd(),
        &[Bind {
            source: BindSource::Path(source.path().to_owned()),
            target: PathBuf::from("work"),
            read_only: false,
        }],
    )
    .err()
    .unwrap();
    assert!(matches!(path_error, ContainerError::UnpinnedAnchoredMountSource { .. }));

    let fifo_path = source.path().join("fifo");
    mkfifo(&fifo_path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let fifo = open_path_file(&fifo_path);
    let error = Container::new_anchored(root.path(), &anchor)
        .unwrap()
        .bind_rw_pinned(&fifo, &fifo_path, "/work")
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("unsupported anchored mount source"));
}

#[test]
fn anchored_bind_apis_require_absolute_source_and_guest_paths() {
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let anchor = open_path_directory(root.path());
    let pinned = open_path_directory(source.path());

    for result in [
        Container::new_anchored(root.path(), &anchor)
            .unwrap()
            .bind_rw_from_root("install", "/install"),
        Container::new_anchored(root.path(), &anchor)
            .unwrap()
            .bind_rw_from_root("/install", "install"),
        Container::new_anchored(root.path(), &anchor)
            .unwrap()
            .bind_rw_pinned(&pinned, source.path(), "work"),
    ] {
        let error = result.err().expect("relative anchored path must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("invalid anchored mount target"));
    }
}

#[test]
fn sealed_resolver_file_has_exact_metadata_seals_and_cleanup() {
    let file = sealed_resolver_file(b"nameserver 192.0.2.1\n").unwrap();
    let fd = file.as_raw_fd();
    let stat = descriptor_stat(fd).unwrap();
    assert_eq!(stat.st_mode & nix::libc::S_IFMT, nix::libc::S_IFREG);
    assert_eq!(stat.st_mode & 0o777, 0o644);
    assert_eq!(stat.st_size, b"nameserver 192.0.2.1\n".len() as i64);
    // SAFETY: fd is a live memfd and F_GET_SEALS takes no third argument.
    let seals = unsafe { nix::libc::fcntl(fd, nix::libc::F_GET_SEALS) };
    let required =
        nix::libc::F_SEAL_WRITE | nix::libc::F_SEAL_GROW | nix::libc::F_SEAL_SHRINK | nix::libc::F_SEAL_SEAL;
    assert_eq!(seals & required, required);
    let mutation = b"mutation";
    // SAFETY: mutation is live for the write and fd denotes the sealed
    // memfd. The syscall must reject the write without reading elsewhere.
    assert_eq!(
        unsafe { nix::libc::write(fd, mutation.as_ptr().cast(), mutation.len()) },
        -1
    );
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(nix::libc::EPERM));
    drop(file);
    assert_eq!(fcntl(fd, FcntlArg::F_GETFD), Err(Errno::EBADF));
}

#[test]
fn resolver_stability_witness_detects_content_metadata_change() {
    let temporary = tempfile::NamedTempFile::new().unwrap();
    fs::write(temporary.path(), b"first").unwrap();
    let file = fs::File::open(temporary.path()).unwrap();
    let before = descriptor_stat(file.as_raw_fd()).unwrap();
    let same = descriptor_stat(file.as_raw_fd()).unwrap();
    assert!(resolver_stat_stable(&before, &same));
    fs::write(temporary.path(), b"different-size").unwrap();
    let after = descriptor_stat(file.as_raw_fd()).unwrap();
    assert!(!resolver_stat_stable(&before, &after));
}

#[test]
fn raw_clone_child_panic_is_contained_and_reported() {
    let mut sync = SyncSocket::new().unwrap();
    let error_writer = sync.child.take().unwrap();
    let exit_code = contain_raw_clone_child_panic(error_writer, || -> i32 {
        panic!("panic must not unwind through the raw clone boundary")
    });
    assert_eq!(exit_code, 1);
    let message = read_child_error(sync.supervisor_fd()).unwrap();
    assert_eq!(
        message,
        "raw fork-like clone child panicked; payload setup was aborted before returning through the cloned parent stack"
    );
}

#[test]
fn child_error_read_does_not_wait_for_a_leaked_descendant_socket() {
    let mut sync = SyncSocket::new().unwrap();
    let child = sync.child.take().unwrap();
    let leaked_child = duplicate_cloexec(child).unwrap();
    assert_eq!(send_packet_no_signal(child, b"bounded child error").unwrap(), 19);
    close_sync_endpoint(child).unwrap();

    let result = read_child_error(sync.supervisor_fd()).unwrap();
    assert_eq!(result, "bounded child error");
    drop(leaked_child);
}

#[test]
fn anchored_resolver_target_uses_the_descriptor_not_the_replaced_label() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let pinned = temporary.path().join("pinned");
    fs::create_dir_all(root.join("etc")).unwrap();
    fs::write(root.join("etc/resolv.conf"), b"authenticated placeholder").unwrap();
    let anchor = open_path_directory(&root);
    fs::rename(&root, &pinned).unwrap();
    fs::create_dir_all(root.join("etc")).unwrap();
    fs::write(root.join("etc/resolv.conf"), b"replacement").unwrap();

    let target = open_anchored_resolver_target(anchor.as_raw_fd()).unwrap();
    let target_stat = descriptor_stat(target.as_raw_fd()).unwrap();
    let expected = fs::metadata(pinned.join("etc/resolv.conf")).unwrap();

    assert_eq!(target_stat.st_dev as u64, expected.dev());
    assert_eq!(target_stat.st_ino as u64, expected.ino());
    assert_eq!(
        fs::read(pinned.join("etc/resolv.conf")).unwrap(),
        b"authenticated placeholder"
    );
    assert_eq!(fs::read(root.join("etc/resolv.conf")).unwrap(), b"replacement");
}

#[test]
fn anchored_resolver_rejects_fifo_and_device_targets_without_opening_data() {
    let fifo_root = tempfile::tempdir().unwrap();
    fs::create_dir(fifo_root.path().join("etc")).unwrap();
    mkfifo(&fifo_root.path().join("etc/resolv.conf"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let fifo_anchor = open_path_directory(fifo_root.path());
    assert!(matches!(
        open_anchored_resolver_target(fifo_anchor.as_raw_fd()),
        Err(ContainerError::UnsafeResolverTarget { mode, .. }) if mode == nix::libc::S_IFIFO
    ));

    let device = open_path_file(Path::new("/dev/null"));
    assert!(matches!(
        validate_resolver_target(device.as_raw_fd(), Path::new("etc/resolv.conf")),
        Err(ContainerError::UnsafeResolverTarget { mode, .. }) if mode == nix::libc::S_IFCHR
    ));

    let hardlink_root = tempfile::tempdir().unwrap();
    fs::create_dir(hardlink_root.path().join("etc")).unwrap();
    let target = hardlink_root.path().join("etc/resolv.conf");
    let alias = hardlink_root.path().join("resolver-alias");
    fs::write(&target, b"do not mutate").unwrap();
    fs::hard_link(&target, &alias).unwrap();
    let hardlink_anchor = open_path_directory(hardlink_root.path());
    let hardlink_descriptor = open_anchored_resolver_target(hardlink_anchor.as_raw_fd()).unwrap();
    let hardlink_stat = descriptor_stat(hardlink_descriptor.as_raw_fd()).unwrap();
    assert_eq!(hardlink_stat.st_nlink, 2);
    assert_eq!(fs::read(&target).unwrap(), b"do not mutate");
    assert_eq!(fs::read(&alias).unwrap(), b"do not mutate");
}


