#[test]
fn bounded_tmpfs_on_a_read_only_root_enforces_exact_byte_and_inode_ceilings() {
    let page_size = unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) };
    assert!(page_size > 0);
    let size_bytes = u64::try_from(page_size).unwrap() * 3;
    let inode_limit = 8;
    let limits = TmpfsLimits::new(size_bytes, inode_limit).unwrap();
    let root = tempfile::tempdir().unwrap();

    // Deliberately leave /tmp absent: setup must prepare the mountpoint
    // before recursively sealing this root read-only.
    let result = Container::new(root.path())
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Bounded(limits),
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault)
        .run::<io::Error>(move || exercise_bounded_tmpfs(size_bytes, inode_limit));

    match result {
        Ok(()) => {
            assert!(root.path().join("tmp").is_dir());
            assert_eq!(std::fs::read_dir(root.path().join("tmp")).unwrap().count(), 0);
        }
        Err(error) => {
            let classification = classify_bounded_tmpfs_activation_unavailable(&error, root.path());
            if skip_activation_capability_denial("live bounded-tmpfs test", classification, &error) {
                return;
            }
            panic!("live bounded-tmpfs test failed: {error}");
        }
    }
}

#[test]
fn anchored_bounded_tmpfs_enforces_the_same_exact_ceilings() {
    let page_size = unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) };
    assert!(page_size > 0);
    let size_bytes = u64::try_from(page_size).unwrap() * 3;
    let inode_limit = 8;
    let limits = TmpfsLimits::new(size_bytes, inode_limit).unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("tmp")).unwrap();
    let anchor = open_path_directory(root.path());

    let result = anchored_container(root.path(), &anchor)
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Bounded(limits),
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault)
        .run::<io::Error>(move || exercise_bounded_tmpfs(size_bytes, inode_limit));

    match result {
        Ok(()) => assert_eq!(std::fs::read_dir(root.path().join("tmp")).unwrap().count(), 0),
        Err(error) => {
            let classification = classify_anchored_activation_unavailable(&error, root.path())
                .or_else(|| classify_bounded_tmpfs_activation_unavailable(&error, root.path()));
            if skip_activation_capability_denial("live anchored bounded-tmpfs test", classification, &error) {
                return;
            }
            panic!("live anchored bounded-tmpfs test failed: {error}");
        }
    }
}

#[test]
fn non_page_aligned_bounded_tmpfs_is_rejected_on_path_activation() {
    let page_size = unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) };
    assert!(page_size > 0);
    let page_size = u64::try_from(page_size).unwrap();
    let requested_size = page_size + 1;
    let inode_limit = 8;
    let limits = TmpfsLimits::new(requested_size, inode_limit).unwrap();
    let root = tempfile::tempdir().unwrap();

    let result = Container::new(root.path())
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Bounded(limits),
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault)
        .run::<io::Error>(|| Ok(()));

    assert_live_tmpfs_normalization_rejected(
        result,
        root.path(),
        requested_size,
        page_size * 2,
        inode_limit,
        false,
        "live path tmpfs-normalization test",
    );
}

#[test]
fn non_page_aligned_bounded_tmpfs_is_rejected_on_anchored_activation() {
    let page_size = unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) };
    assert!(page_size > 0);
    let page_size = u64::try_from(page_size).unwrap();
    let requested_size = page_size + 1;
    let inode_limit = 8;
    let limits = TmpfsLimits::new(requested_size, inode_limit).unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("tmp")).unwrap();
    let anchor = open_path_directory(root.path());

    let result = anchored_container(root.path(), &anchor)
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Bounded(limits),
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault)
        .run::<io::Error>(|| Ok(()));

    assert_live_tmpfs_normalization_rejected(
        result,
        root.path(),
        requested_size,
        page_size * 2,
        inode_limit,
        true,
        "live anchored tmpfs-normalization test",
    );
}

#[test]
fn minimal_dev_is_read_only_and_exact_on_the_path_activation() {
    let root = tempfile::tempdir().unwrap();
    let result = Container::new(root.path())
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::Minimal,
        })
        .loopback(LoopbackPolicy::KernelDefault)
        .run::<io::Error>(exercise_read_only_minimal_dev);

    match result {
        Ok(()) => {
            assert!(root.path().join("dev").is_dir());
            assert_eq!(std::fs::read_dir(root.path().join("dev")).unwrap().count(), 0);
        }
        Err(error) => {
            let classification = classify_minimal_dev_activation_unavailable(&error, root.path());
            if skip_activation_capability_denial("live path minimal-dev test", classification, &error) {
                return;
            }
            panic!("live path minimal-dev test failed: {error}");
        }
    }
}

#[test]
fn minimal_dev_is_read_only_and_exact_on_anchored_activation() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("dev")).unwrap();
    let anchor = open_path_directory(root.path());
    let result = anchored_container(root.path(), &anchor)
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::Minimal,
        })
        .loopback(LoopbackPolicy::KernelDefault)
        .run::<io::Error>(exercise_read_only_minimal_dev);

    match result {
        Ok(()) => assert_eq!(std::fs::read_dir(root.path().join("dev")).unwrap().count(), 0),
        Err(error) => {
            let classification = classify_anchored_activation_unavailable(&error, root.path())
                .or_else(|| classify_minimal_dev_activation_unavailable(&error, root.path()));
            if skip_activation_capability_denial("live anchored minimal-dev test", classification, &error) {
                return;
            }
            panic!("live anchored minimal-dev test failed: {error}");
        }
    }
}

#[test]
fn read_only_root_is_enforced_by_the_live_kernel_mount_and_capability_paths() {
    let root = tempfile::tempdir().unwrap();
    let writable = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("locked")).unwrap();
    fs::write(root.path().join("locked/input"), b"immutable").unwrap();

    let result = Container::new(root.path())
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault)
        .bind_rw(writable.path(), "/work")
        .run::<io::Error>(|| {
            require_errno(
                fs::write("/locked/initial-mutation", b"rejected"),
                Errno::EROFS,
                "write undeclared root path before remount attempts",
            )?;
            fs::write("/work/result", b"writable bind")?;
            require_payload_security_boundary()?;

            let remount = nix::mount::mount::<str, str, str, str>(
                None,
                "/",
                None,
                nix::mount::MsFlags::MS_BIND | nix::mount::MsFlags::MS_REMOUNT,
                None,
            );
            if !matches!(remount, Err(Errno::EPERM)) {
                return Err(io::Error::other(format!(
                    "root remount without CAP_SYS_ADMIN did not fail with EPERM: {remount:?}"
                )));
            }

            match set_mount_access(Path::new("/"), false, true) {
                Err(ContainerError::Mount {
                    source: Errno::EPERM, ..
                }) => {}
                Err(error) => {
                    return Err(io::Error::other(format!(
                        "mount_setattr write-enable failed unexpectedly: {error}"
                    )));
                }
                Ok(()) => {
                    return Err(io::Error::other(
                        "mount_setattr write-enable succeeded without CAP_SYS_ADMIN",
                    ));
                }
            }

            require_errno(
                fs::write("/locked/post-remount-mutation", b"rejected"),
                Errno::EROFS,
                "write undeclared root path after remount attempts",
            )
        });

    match result {
        Ok(()) => {
            assert_eq!(fs::read(writable.path().join("result")).unwrap(), b"writable bind");
            assert!(!root.path().join("locked/initial-mutation").exists());
            assert!(!root.path().join("locked/post-remount-mutation").exists());
        }
        Err(error) if host_denied_user_namespace_setup(&error) => {
            eprintln!("SKIP live read-only-root kernel test: host denied user-namespace credential setup: {error}");
        }
        Err(error) => panic!("live read-only-root kernel test failed: {error}"),
    }
}

#[test]
fn anchored_root_symlink_substitution_fails_before_payload_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let original = temporary.path().join("original-root");
    let replacement = temporary.path().join("replacement-root");
    let payload_marker = temporary.path().join("payload-ran");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("identity"), b"authenticated-root").unwrap();

    let anchor = open_path_directory(&root);
    let container = anchored_container(&root, &anchor)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault);

    fs::rename(&root, &original).unwrap();
    fs::create_dir(&replacement).unwrap();
    fs::write(replacement.join("identity"), b"replacement-root").unwrap();
    std::os::unix::fs::symlink(&replacement, &root).unwrap();

    let result = container.run::<io::Error>(|| fs::write(&payload_marker, b"payload ran"));
    assert!(matches!(
        result,
        Err(ContainerRunError::Failure { message })
            if message.contains("reopen anchored container root")
                && message.contains("Too many levels of symbolic links")
    ));
    assert!(!payload_marker.exists());
    assert_eq!(fs::read(original.join("identity")).unwrap(), b"authenticated-root");
    assert_eq!(fs::read(replacement.join("identity")).unwrap(), b"replacement-root");
}

#[test]
fn anchored_root_and_bind_locators_rebind_in_the_live_child_namespace() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let external_work = temporary.path().join("external-work");
    fs::create_dir_all(root.join("install")).unwrap();
    fs::create_dir(root.join("work")).unwrap();
    fs::create_dir(root.join("locked")).unwrap();
    fs::write(root.join("locked/input"), b"immutable").unwrap();
    fs::create_dir(&external_work).unwrap();

    let anchor = open_path_directory(&root);
    let work = open_path_directory(&external_work);
    let container = anchored_container(&root, &anchor)
        .bind_rw_from_root("/install", "/install")
        .unwrap()
        .bind_rw_pinned(exact_locator(&external_work, &work), "/work")
        .unwrap()
        .root_filesystem(RootFilesystemPolicy::ReadOnly)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault);

    let result = container.run::<io::Error>(|| {
        fs::write("/install/result", b"authenticated install")?;
        fs::write("/work/result", b"external work")?;
        require_errno(
            fs::write("/locked/mutation", b"rejected"),
            Errno::EROFS,
            "mutate undeclared anchored root path",
        )?;
        require_payload_security_boundary()
    });

    match result {
        Ok(()) => {
            assert_eq!(fs::read(root.join("install/result")).unwrap(), b"authenticated install");
            assert_eq!(fs::read(external_work.join("result")).unwrap(), b"external work");
            assert!(!root.join("locked/mutation").exists());
        }
        Err(error) => {
            let classification = classify_anchored_activation_unavailable(&error, &root);
            if let Some(classification) = classification
                && std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()
                    != Some(std::ffi::OsStr::new("1"))
            {
                eprintln!(
                    "SKIP anchored child-namespace rebind test: required host capability unavailable: {classification}: {error}"
                );
                return;
            }
            panic!("anchored child-namespace rebind test failed: {error}");
        }
    }
}

#[test]
fn anchored_payload_error_transport_is_bounded_and_completes() {
    let root = tempfile::tempdir().unwrap();
    let anchor = open_path_directory(root.path());
    let container = anchored_container(root.path(), &anchor)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault);
    let result = container.run::<io::Error>(|| Err(io::Error::other("x".repeat(1024 * 1024))));

    match result {
        Err(ContainerRunError::Failure { message }) if message.starts_with("run: ") => {
            assert_eq!(message.len(), MAX_CHILD_ERROR_BYTES);
        }
        Err(error) => {
            let classification = classify_anchored_activation_unavailable(&error, root.path());
            if let Some(classification) = classification
                && std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()
                    != Some(std::ffi::OsStr::new("1"))
            {
                eprintln!(
                    "SKIP anchored bounded-error test: required host capability unavailable: {classification}: {error}"
                );
                return;
            }
            panic!("anchored bounded-error test failed: {error}");
        }
        Ok(()) => panic!("anchored payload unexpectedly accepted an error"),
    }
}

#[test]
fn anchored_root_clone_excludes_undeclared_nested_mounts() {
    const PROC_SUPER_MAGIC: nix::libc::c_long = 0x0000_9fa0;

    let anchor = open_path_directory(Path::new("/"));
    let label = PathBuf::from("/");
    let container = anchored_container(&label, &anchor)
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault);
    drop(anchor);

    let result = container.run::<io::Error>(|| {
        // SAFETY: the path is static and NUL terminated; statfs points to
        // a fully initialized output object for the duration of the call.
        let mut stat: nix::libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { nix::libc::statfs(c"/proc".as_ptr(), &mut stat) } == -1 {
            return Err(io::Error::last_os_error());
        }
        if stat.f_type == PROC_SUPER_MAGIC {
            return Err(io::Error::other(format!(
                "undeclared nested /proc mount was imported: filesystem magic={:#x}",
                stat.f_type
            )));
        }
        match fs::metadata("/proc/self/stat") {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(io::Error::other("undeclared nested /proc contents were imported")),
        }
        Ok(())
    });

    match result {
        Ok(()) => {}
        Err(error) => {
            let classification = classify_anchored_activation_unavailable(&error, &label);
            if let Some(classification) = classification
                && std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()
                    != Some(std::ffi::OsStr::new("1"))
            {
                eprintln!(
                    "SKIP anchored root nested-mount exclusion test: required host capability unavailable: {classification}: {error}"
                );
                return;
            }
            panic!("anchored root nested-mount exclusion test failed: {error}");
        }
    }
}

#[test]
fn anchored_directory_bind_excludes_undeclared_nested_mounts() {
    const PROC_SUPER_MAGIC: nix::libc::c_long = 0x0000_9fa0;

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("import")).unwrap();
    let root_anchor = open_path_directory(root.path());
    let host_root = open_path_directory(Path::new("/"));
    let container = anchored_container(root.path(), &root_anchor)
        .bind_ro_pinned(exact_locator(Path::new("/"), &host_root), "/import")
        .unwrap()
        .pseudo_filesystems(PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        })
        .loopback(LoopbackPolicy::KernelDefault);

    let result = container.run::<io::Error>(|| {
        // SAFETY: the path is static and NUL terminated; statfs points to
        // a fully initialized output object for the duration of the call.
        let mut stat: nix::libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { nix::libc::statfs(c"/import/proc".as_ptr(), &mut stat) } == -1 {
            return Err(io::Error::last_os_error());
        }
        if stat.f_type == PROC_SUPER_MAGIC {
            return Err(io::Error::other(format!(
                "directory bind imported nested /proc mount: filesystem magic={:#x}",
                stat.f_type
            )));
        }
        match fs::metadata("/import/proc/self/stat") {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(io::Error::other(
                "directory bind imported undeclared nested /proc contents",
            )),
        }
    });

    match result {
        Ok(()) => {}
        Err(error) => {
            let classification = classify_anchored_activation_unavailable(&error, root.path());
            if let Some(classification) = classification
                && std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()
                    != Some(std::ffi::OsStr::new("1"))
            {
                eprintln!(
                    "SKIP anchored bind nested-mount exclusion test: required host capability unavailable: {classification}: {error}"
                );
                return;
            }
            panic!("anchored bind nested-mount exclusion test failed: {error}");
        }
    }
}
