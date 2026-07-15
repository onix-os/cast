#[test]
fn frozen_root_normalizes_enforceable_metadata_in_canonical_order() {
    const CHILD: &str = "CAST_FROZEN_ROOT_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        run_frozen_root_materialization_test();
        return;
    }

    // umask is process-global. Run the hostile-umask proof in a dedicated
    // test process so unrelated parallel tests cannot observe it.
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("frozen_root_normalizes_enforceable_metadata_in_canonical_order")
        .arg("--test-threads=1")
        .env(CHILD, "1")
        .status()
        .unwrap();
    assert!(status.success());
}

fn run_frozen_root_materialization_test() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let blit_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&blit_root).unwrap();

    let installation = frozen_test_installation(&installation_root);
    let client = Client::frozen("frozen-root-test", installation, repository::Map::default(), &blit_root).unwrap();
    let isolation_marker = client.installation.isolation_dir().join("must-remain");
    fs::create_dir_all(isolation_marker.parent().unwrap()).unwrap();
    fs::write(&isolation_marker, b"isolation root is out of scope").unwrap();

    let first = package::Id::from("a-frozen-package");
    let second = package::Id::from("z-frozen-package");
    let omitted = package::Id::from("zz-omitted-package");
    let asset_bytes = test_elf(None, 1);
    let mut adversarial_asset_bytes = asset_bytes.clone();
    let last = adversarial_asset_bytes.last_mut().unwrap();
    *last ^= 1;
    let asset_id = xxhash_rust::xxh3::xxh3_128(&asset_bytes);
    let empty_id = 0x99aa_06d3_0147_98d8_6001_c324_468d_497f_u128;
    let layouts = [
        (
            &first,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFDIR | 0o751,
                tag: 0,
                file: StonePayloadLayoutFile::Directory("bin".into()),
            },
        ),
        (
            &first,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(asset_id, "bin/tool".into()),
            },
        ),
        (
            &first,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFLNK | 0o777,
                tag: 0,
                file: StonePayloadLayoutFile::Symlink("tool".into(), "bin/tool-link".into()),
            },
        ),
        (
            &first,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFLNK | 0o777,
                tag: 0,
                file: StonePayloadLayoutFile::Symlink("other-tool".into(), "bin/cross-tool".into()),
            },
        ),
        (
            &first,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFLNK | 0o777,
                tag: 0,
                file: StonePayloadLayoutFile::Symlink("cycle-b".into(), "bin/cycle-a".into()),
            },
        ),
        (
            &first,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFLNK | 0o777,
                tag: 0,
                file: StonePayloadLayoutFile::Symlink("cycle-a".into(), "bin/cycle-b".into()),
            },
        ),
        (
            &first,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o640,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(empty_id, "share/empty".into()),
            },
        ),
        (
            &first,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFDIR | 0o555,
                tag: 0,
                file: StonePayloadLayoutFile::Directory("share/restricted".into()),
            },
        ),
        (
            &first,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(asset_id, "share/restricted/tool".into()),
            },
        ),
        // Identical directory records may be shared by packages.
        (
            &second,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFDIR | 0o751,
                tag: 0,
                file: StonePayloadLayoutFile::Directory("bin".into()),
            },
        ),
        (
            &second,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(asset_id, "bin/other-tool".into()),
            },
        ),
        (
            &omitted,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o600,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(asset_id, "share/omitted".into()),
            },
        ),
    ];
    client
        .layout_db
        .batch_add(layouts.iter().map(|(package, layout)| (*package, layout)))
        .unwrap();

    let asset_path = cache::asset_path(&client.installation, &format!("{asset_id:02x}"));
    fs::create_dir_all(asset_path.parent().unwrap()).unwrap();
    fs::write(&asset_path, &asset_bytes).unwrap();
    fs::set_permissions(&asset_path, Permissions::from_mode(0o640)).unwrap();
    let asset_metadata = fs::metadata(&asset_path).unwrap();

    nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
    const EPOCH: i64 = 1_700_000_123;
    client.discard_frozen_root().unwrap();
    let _materialized = client
        .blit_frozen_root(&[second.clone(), first.clone()], EPOCH)
        .unwrap();

    let tool = blit_root.join("usr/bin/tool");
    let empty = blit_root.join("usr/share/empty");
    let tool_link = blit_root.join("usr/bin/tool-link");
    let timestamped = [
        blit_root.clone(),
        blit_root.join("usr"),
        blit_root.join("usr/bin"),
        blit_root.join("usr/share"),
        blit_root.join("usr/share/restricted"),
        blit_root.join("usr/share/restricted/tool"),
        tool.clone(),
        empty.clone(),
        tool_link.clone(),
        blit_root.join("bin"),
        blit_root.join("sbin"),
        blit_root.join("lib"),
        blit_root.join("lib64"),
        blit_root.join("lib32"),
    ];
    for path in &timestamped {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert_eq!(
            FileTime::from_last_access_time(&metadata).unix_seconds(),
            EPOCH,
            "{path:?}"
        );
        assert_eq!(
            FileTime::from_last_modification_time(&metadata).unix_seconds(),
            EPOCH,
            "{path:?}"
        );
    }
    // This manifest intentionally covers only metadata the materializer
    // can enforce: path, inode type, mode, bytes/link target, atime and
    // mtime. Kernel-assigned inode/dev/ctime/btime are outside the claim.
    let first_manifest = frozen_enforceable_manifest(&blit_root);

    assert_eq!(fs::metadata(&blit_root).unwrap().permissions().mode() & 0o7777, 0o755);
    assert_eq!(
        fs::metadata(blit_root.join("usr/bin")).unwrap().permissions().mode() & 0o7777,
        0o751
    );
    assert_eq!(fs::metadata(&tool).unwrap().permissions().mode() & 0o7777, 0o755);
    assert_eq!(fs::metadata(&empty).unwrap().permissions().mode() & 0o7777, 0o640);
    assert_eq!(
        fs::metadata(blit_root.join("usr/share/restricted"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o555
    );
    assert_eq!(
        fs::read(blit_root.join("usr/share/restricted/tool")).unwrap(),
        asset_bytes
    );
    assert_eq!(fs::read(&tool).unwrap(), asset_bytes);
    assert_eq!(fs::metadata(&tool).unwrap().len(), asset_bytes.len() as u64);
    assert_ne!(
        (asset_metadata.dev(), asset_metadata.ino()),
        (fs::metadata(&tool).unwrap().dev(), fs::metadata(&tool).unwrap().ino())
    );
    assert_eq!(fs::read_link(&tool_link).unwrap(), PathBuf::from("tool"));
    for (source, target) in ROOT_ABI_LINKS {
        assert_eq!(fs::read_link(blit_root.join(target)).unwrap(), PathBuf::from(source));
    }

    assert!(!blit_root.join("usr/share/omitted").exists());
    assert!(!blit_root.join("usr/.stateID").exists());
    assert!(!blit_root.join("usr/lib/os-release").exists());
    assert!(!system_model::snapshot_path(&blit_root).exists());
    assert!(!blit_root.join("etc").exists());
    assert_eq!(fs::read(&isolation_marker).unwrap(), b"isolation root is out of scope");
    assert_eq!(fs::read(&asset_path).unwrap(), asset_bytes);
    assert_eq!(fs::metadata(&asset_path).unwrap().permissions().mode() & 0o7777, 0o640);

    let packages = [second.clone(), first.clone()];
    let tool_binding = FrozenExecutableBinding {
        package: first.clone(),
        path: PathBuf::from("/usr/bin/tool"),
    };
    let tool_guard = client
        .require_frozen_executables(&packages, std::slice::from_ref(&tool_binding))
        .unwrap();
    let retained_tool = blit_root.join("usr/bin/tool-before-substitution");
    fs::rename(&tool, &retained_tool).unwrap();
    fs::write(&tool, &asset_bytes).unwrap();
    fs::set_permissions(&tool, Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        tool_guard.revalidate(),
        Err(Error::FrozenExecutablePathReplaced { package, path })
            if package == first && path == Path::new("/usr/bin/tool")
    ));
    fs::remove_file(&tool).unwrap();
    fs::rename(&retained_tool, &tool).unwrap();
    drop(tool_guard);

    let outside = FrozenExecutableBinding {
        package: omitted.clone(),
        path: PathBuf::from("/usr/share/omitted"),
    };
    assert!(matches!(
        client.require_frozen_executables(&packages, &[outside]),
        Err(Error::FrozenExecutableProviderOutsideClosure { package, path })
            if package == omitted && path == Path::new("/usr/share/omitted")
    ));

    let wrong_provider = FrozenExecutableBinding {
        package: second.clone(),
        path: PathBuf::from("/usr/bin/tool"),
    };
    assert!(matches!(
        client.require_frozen_executables(&packages, &[wrong_provider]),
        Err(Error::MissingFrozenExecutableLayout { package, path })
            if package == second && path == Path::new("/usr/bin/tool")
    ));

    let cross_provider_symlink = FrozenExecutableBinding {
        package: first.clone(),
        path: PathBuf::from("/usr/bin/cross-tool"),
    };
    let cross_provider_guard = client
        .require_frozen_executables(&packages, std::slice::from_ref(&cross_provider_symlink))
        .unwrap();
    cross_provider_guard.revalidate().unwrap();
    drop(cross_provider_guard);

    let cyclic_symlink = FrozenExecutableBinding {
        package: first.clone(),
        path: PathBuf::from("/usr/bin/cycle-a"),
    };
    assert!(matches!(
        client.require_frozen_executables(&packages, &[cyclic_symlink]),
        Err(Error::FrozenExecutableSymlinkCycle { package, path })
            if package == first && path == Path::new("/usr/bin/cycle-a")
    ));

    let symlink_binding = FrozenExecutableBinding {
        package: first.clone(),
        path: PathBuf::from("/usr/bin/tool-link"),
    };
    let _guard = client
        .require_frozen_executables(&packages, std::slice::from_ref(&symlink_binding))
        .unwrap();
    fs::remove_file(&tool_link).unwrap();
    symlink("../share/empty", &tool_link).unwrap();
    assert!(matches!(
        client.require_frozen_executables(&packages, &[symlink_binding]),
        Err(Error::FrozenExecutableSymlinkTargetMismatch { package, path, expected, actual })
            if package == first
                && path == Path::new("/usr/bin/tool-link")
                && expected == "tool"
                && actual == OsString::from("../share/empty")
    ));
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

    let non_executable = FrozenExecutableBinding {
        package: first.clone(),
        path: PathBuf::from("/usr/share/empty"),
    };
    assert!(matches!(
        client.require_frozen_executables(&packages, &[non_executable]),
        Err(Error::FrozenExecutableLayoutNotExecutable { package, path, mode })
            if package == first
                && path == Path::new("/usr/share/empty")
                && mode == nix::libc::S_IFREG | 0o640
    ));

    let invalid_path = FrozenExecutableBinding {
        package: first.clone(),
        path: PathBuf::from("/usr/bin/../bin/tool"),
    };
    assert!(matches!(
        client.require_frozen_executables(&packages, &[invalid_path]),
        Err(Error::InvalidFrozenExecutablePath { package, path })
            if package == first && path == Path::new("/usr/bin/../bin/tool")
    ));

    fs::set_permissions(&tool, Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
        Err(Error::FrozenExecutableModeMismatch { package, path, expected, actual })
            if package == first
                && path == Path::new("/usr/bin/tool")
                && expected == nix::libc::S_IFREG | 0o755
                && actual == nix::libc::S_IFREG | 0o700
    ));
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

    let hardlink = blit_root.join("usr/bin/tool-hardlink");
    fs::hard_link(&tool, &hardlink).unwrap();
    assert!(matches!(
        client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
        Err(Error::FrozenExecutableNotIndependentRegular { package, path, links: 2, .. })
            if package == first && path == Path::new("/usr/bin/tool")
    ));
    fs::remove_file(&hardlink).unwrap();
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

    let oversized = fs::OpenOptions::new().write(true).open(&tool).unwrap();
    oversized.set_len(MAX_FROZEN_EXECUTABLE_BYTES + 1).unwrap();
    drop(oversized);
    assert!(matches!(
        client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
        Err(Error::FrozenExecutableByteLimit { package, path, limit, actual })
            if package == first
                && path == Path::new("/usr/bin/tool")
                && limit == MAX_FROZEN_EXECUTABLE_BYTES
                && actual == MAX_FROZEN_EXECUTABLE_BYTES + 1
    ));
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

    fs::write(&tool, &adversarial_asset_bytes).unwrap();
    assert_eq!(fs::metadata(&tool).unwrap().len(), asset_bytes.len() as u64);
    assert!(matches!(
        client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
        Err(Error::FrozenExecutableDigestMismatch { package, path, .. })
            if package == first && path == Path::new("/usr/bin/tool")
    ));
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

    let runtime_symlink = blit_root.join("usr/bin/tool-runtime-link");
    fs::remove_file(&tool).unwrap();
    symlink("tool-runtime-link", &tool).unwrap();
    fs::write(&runtime_symlink, &asset_bytes).unwrap();
    fs::set_permissions(&runtime_symlink, Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
        Err(Error::OpenFrozenExecutable { package, path, .. })
            if package == first && path == Path::new("/usr/bin/tool")
    ));
    fs::remove_file(&runtime_symlink).unwrap();
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

    let mut changed_after_digest = false;
    let error = require_frozen_executables(
        &client,
        test_materialized_frozen_root(&blit_root).unwrap(),
        &packages,
        std::slice::from_ref(&tool_binding),
        |binding, checkpoint| {
            if checkpoint == FrozenExecutableCheckpoint::AfterDigest && !changed_after_digest {
                assert_eq!(binding, &tool_binding);
                fs::write(&tool, &adversarial_asset_bytes).unwrap();
                fs::set_permissions(&tool, Permissions::from_mode(0o700)).unwrap();
                changed_after_digest = true;
            }
        },
    )
    .unwrap_err();
    assert!(changed_after_digest);
    assert!(matches!(
        error,
        Error::FrozenExecutableChanged { package, path }
            if package == first && path == Path::new("/usr/bin/tool")
    ));
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

    let replacement = blit_root.join("usr/bin/tool-replacement");
    fs::write(&replacement, &asset_bytes).unwrap();
    fs::set_permissions(&replacement, Permissions::from_mode(0o755)).unwrap();
    let mut replaced_before_reopen = false;
    let error = require_frozen_executables(
        &client,
        test_materialized_frozen_root(&blit_root).unwrap(),
        &packages,
        std::slice::from_ref(&tool_binding),
        |binding, checkpoint| {
            if checkpoint == FrozenExecutableCheckpoint::BeforeReopen && !replaced_before_reopen {
                assert_eq!(binding, &tool_binding);
                fs::rename(&replacement, &tool).unwrap();
                replaced_before_reopen = true;
            }
        },
    )
    .unwrap_err();
    assert!(replaced_before_reopen);
    assert!(matches!(
        error,
        Error::FrozenExecutablePathReplaced { package, path }
            if package == first && path == Path::new("/usr/bin/tool")
    ));
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

    // A second materialization reverses caller and database order, changes
    // the process umask, and still reproduces all enforceable metadata.
    fs::write(&tool, b"mutated build root").unwrap();
    fs::set_permissions(&tool, Permissions::from_mode(0o600)).unwrap();
    client
        .layout_db
        .batch_add(layouts.iter().rev().map(|(package, layout)| (*package, layout)))
        .unwrap();
    nix::sys::stat::umask(Mode::from_bits_truncate(0o022));
    client.discard_frozen_root().unwrap();
    let _materialized = client
        .blit_frozen_root(&[first.clone(), second.clone()], EPOCH)
        .unwrap();
    assert_eq!(frozen_enforceable_manifest(&blit_root), first_manifest);
    assert_eq!(fs::read(&tool).unwrap(), asset_bytes);
    assert_eq!(fs::metadata(&tool).unwrap().permissions().mode() & 0o7777, 0o755);
    assert_eq!(
        FileTime::from_last_modification_time(&fs::metadata(&tool).unwrap()).unix_seconds(),
        EPOCH
    );
    make_tree_removable(&blit_root).unwrap();
}

fn frozen_enforceable_manifest(root: &Path) -> Vec<(String, &'static str, u32, i64, i64, Vec<u8>)> {
    fn visit(root: &Path, path: &Path, manifest: &mut Vec<(String, &'static str, u32, i64, i64, Vec<u8>)>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let (kind, content) = if metadata.file_type().is_symlink() {
            (
                "symlink",
                fs::read_link(path).unwrap().to_string_lossy().into_owned().into_bytes(),
            )
        } else if metadata.is_dir() {
            ("directory", Vec::new())
        } else {
            ("regular", fs::read(path).unwrap())
        };
        let relative = path.strip_prefix(root).unwrap();
        manifest.push((
            if relative.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                relative.to_string_lossy().into_owned()
            },
            kind,
            metadata.mode() & 0o7777,
            metadata.atime(),
            metadata.mtime(),
            content,
        ));

        if metadata.is_dir() {
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, manifest);
            }
        }
    }

    let mut manifest = Vec::new();
    visit(root, root, &mut manifest);
    manifest
}

#[test]
fn frozen_root_rejects_non_directory_collisions_before_touching_destination() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let blit_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&blit_root).unwrap();
    let marker = blit_root.join("untouched");
    fs::write(&marker, b"original root").unwrap();

    let installation = frozen_test_installation(&installation_root);
    let client = Client::frozen(
        "frozen-collision-test",
        installation,
        repository::Map::default(),
        &blit_root,
    )
    .unwrap();
    let first = package::Id::from("a-collision");
    let second = package::Id::from("z-collision");
    let first_layout = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o644,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(1, "bin/conflict".into()),
    };
    let second_layout = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(2, "bin/conflict".into()),
    };
    client
        .layout_db
        .batch_add([(&second, &second_layout), (&first, &first_layout)])
        .unwrap();

    let error = client
        .blit_frozen_root(&[second.clone(), first.clone()], 1_700_000_000)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::FrozenPathCollision { path, first: found_first, second: found_second }
            if path == "/usr/bin/conflict" && found_first == first && found_second == second
    ));
    assert_eq!(fs::read(marker).unwrap(), b"original root");
}
