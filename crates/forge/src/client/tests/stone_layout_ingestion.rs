#[derive(Debug, Clone, Copy)]
enum TestStoneLayoutKind {
    Regular,
    Symlink,
    Directory,
    CharacterDevice,
    BlockDevice,
    Fifo,
    Socket,
    Unknown,
}

const ALL_STONE_LAYOUT_KINDS: [TestStoneLayoutKind; 8] = [
    TestStoneLayoutKind::Regular,
    TestStoneLayoutKind::Symlink,
    TestStoneLayoutKind::Directory,
    TestStoneLayoutKind::CharacterDevice,
    TestStoneLayoutKind::BlockDevice,
    TestStoneLayoutKind::Fifo,
    TestStoneLayoutKind::Socket,
    TestStoneLayoutKind::Unknown,
];

const UNSUPPORTED_STONE_LAYOUT_KINDS: [TestStoneLayoutKind; 5] = [
    TestStoneLayoutKind::CharacterDevice,
    TestStoneLayoutKind::BlockDevice,
    TestStoneLayoutKind::Fifo,
    TestStoneLayoutKind::Socket,
    TestStoneLayoutKind::Unknown,
];

fn test_stone_layout(kind: TestStoneLayoutKind, target: impl Into<AStr>) -> StonePayloadLayoutRecord {
    let target = target.into();
    let file = match kind {
        TestStoneLayoutKind::Regular => StonePayloadLayoutFile::Regular(42, target),
        TestStoneLayoutKind::Symlink => StonePayloadLayoutFile::Symlink("tool".into(), target),
        TestStoneLayoutKind::Directory => StonePayloadLayoutFile::Directory(target),
        TestStoneLayoutKind::CharacterDevice => StonePayloadLayoutFile::CharacterDevice(target),
        TestStoneLayoutKind::BlockDevice => StonePayloadLayoutFile::BlockDevice(target),
        TestStoneLayoutKind::Fifo => StonePayloadLayoutFile::Fifo(target),
        TestStoneLayoutKind::Socket => StonePayloadLayoutFile::Socket(target),
        TestStoneLayoutKind::Unknown => StonePayloadLayoutFile::Unknown("opaque".into(), target),
    };
    StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: 0,
        tag: 0,
        file,
    }
}

#[test]
fn stone_layout_ingestion_confines_every_inode_variant_to_canonical_usr_relative_targets() {
    let package = package::Id::from("layout-path-policy");
    for (index, kind) in ALL_STONE_LAYOUT_KINDS.into_iter().enumerate() {
        let target = format!("share/layout-kind-{index}");
        let valid = test_stone_layout(kind, target.clone());
        require_usr_relative_stone_layout(&package, &valid).unwrap();
        assert_eq!(
            PendingFile {
                id: package.clone(),
                layout: valid
            }
            .path()
            .to_string(),
            format!("/usr/{target}")
        );

        let absolute = format!("/usr/{target}");
        let invalid = test_stone_layout(kind, absolute.clone());
        assert!(matches!(
            require_usr_relative_stone_layout(&package, &invalid),
            Err(Error::InvalidStoneLayoutTarget {
                package: rejected_package,
                target: rejected_target,
                reason: "the target is absolute",
            }) if rejected_package == package && rejected_target == absolute
        ));
        assert!(matches!(
            vfs(vec![(package.clone(), invalid)]),
            Err(Error::InvalidStoneLayoutTarget {
                package: rejected_package,
                target: rejected_target,
                reason: "the target is absolute",
            }) if rejected_package == package && rejected_target == absolute
        ));

        for reserved in [
            ".cast-state-id.tmp",
            ".cast-tree-id",
            ".cast-tree-id.tmp",
            ".stateID/forged-child",
            "lib/os-release",
            "lib/os-release/forged-child",
            "lib/system-model.glu",
            "lib/system-model.glu/forged-child",
        ] {
            let invalid = test_stone_layout(kind, reserved);
            assert!(matches!(
                require_usr_relative_stone_layout(&package, &invalid),
                Err(Error::InvalidStoneLayoutTarget {
                    package: rejected_package,
                    target,
                    reason: "the target is reserved for Cast system metadata",
                }) if rejected_package == package && target == reserved
            ));
            assert!(matches!(
                vfs(vec![(package.clone(), invalid)]),
                Err(Error::InvalidStoneLayoutTarget {
                    package: rejected_package,
                    target,
                    reason: "the target is reserved for Cast system metadata",
                }) if rejected_package == package && target == reserved
            ));
        }
    }
}

#[test]
fn materialization_rejects_decodable_special_layouts_with_exact_package_and_path() {
    let package = package::Id::from("unsupported-layout");
    for (index, kind) in UNSUPPORTED_STONE_LAYOUT_KINDS.into_iter().enumerate() {
        let target = format!("share/special-{index}");
        let layout = test_stone_layout(kind, target.clone());

        require_usr_relative_stone_layout(&package, &layout).unwrap();
        assert!(matches!(
            vfs(vec![(package.clone(), layout)]),
            Err(Error::UnsupportedFrozenLayout {
                package: rejected_package,
                path,
            }) if rejected_package == package && path == format!("/usr/{target}")
        ));
    }
}

#[test]
fn stone_layout_ingestion_rejects_every_noncanonical_or_escaping_target() {
    let package = package::Id::from("invalid-layout-path");
    let cases = [
        ("", "the target is empty"),
        ("/", "the target is absolute"),
        ("/usr", "the target is absolute"),
        ("/usr/bin/tool", "the target is absolute"),
        ("/etc/passwd", "the target is absolute"),
        (".", "the target contains a dot component"),
        ("..", "the target contains a dot component"),
        ("./bin/tool", "the target contains a dot component"),
        ("bin/./tool", "the target contains a dot component"),
        ("bin/../tool", "the target contains a dot component"),
        ("bin//tool", "the target contains a repeated separator"),
        ("bin/tool/", "the target has a trailing separator"),
        ("bin/\0tool", "the target contains an ASCII control byte"),
        ("bin/\ntool", "the target contains an ASCII control byte"),
        ("bin/\u{7f}tool", "the target contains an ASCII control byte"),
    ];

    for (target, expected_reason) in cases {
        let layout = test_stone_layout(TestStoneLayoutKind::Regular, target);
        assert!(
            matches!(
                require_usr_relative_stone_layout(&package, &layout),
                Err(Error::InvalidStoneLayoutTarget {
                    package: rejected_package,
                    target: rejected_target,
                    reason,
                }) if rejected_package == package && rejected_target == target && reason == expected_reason
            ),
            "accepted invalid Stone layout target {target:?}"
        );
    }

    let oversized_absolute = format!("/{}", "工".repeat(MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES));
    let layout = test_stone_layout(TestStoneLayoutKind::Regular, oversized_absolute);
    let Error::InvalidStoneLayoutTarget {
        target,
        reason: "the target is absolute",
        ..
    } = require_usr_relative_stone_layout(&package, &layout).unwrap_err()
    else {
        panic!("oversized absolute target returned the wrong error");
    };
    assert!(target.ends_with('…'));
    assert!(target.len() <= MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES + '…'.len_utf8());
}

#[test]
fn stone_layout_ingestion_accepts_utf8_and_exact_linux_path_boundaries() {
    // Layout targets are AStr values, so non-UTF-8 bytes cannot enter this
    // validator. Non-ASCII UTF-8 remains part of the admitted domain.
    for target in [
        "bin/tool",
        ".hidden",
        ".cast-state-id.tmp-old",
        ".cast-tree-id-old",
        ".cast-tree-id.tmp-old",
        ".stateID.old/child",
        "lib/os-info.json",
        "lib/os-release.local",
        "lib/system-model.glu.old",
        "share/Grüße/工具",
        "usr/bin/nested",
    ] {
        require_usr_relative_stone_target(target).unwrap();
    }

    let exact_component = "a".repeat(MAX_STONE_LAYOUT_COMPONENT_BYTES);
    require_usr_relative_stone_target(&exact_component).unwrap();
    assert_eq!(
        require_usr_relative_stone_target(&format!("{exact_component}a")),
        Err("a target component exceeds Linux NAME_MAX")
    );

    let exact_depth = std::iter::repeat_n("a", MAX_FROZEN_LAYOUT_PATH_COMPONENTS - 1)
        .collect::<Vec<_>>()
        .join("/");
    require_usr_relative_stone_target(&exact_depth).unwrap();
    let excessive_depth = format!("{exact_depth}/a");
    assert_eq!(
        require_usr_relative_stone_target(&excessive_depth),
        Err("the materialized path is too deep")
    );

    let exact_path = std::iter::repeat_n("a".repeat(MAX_STONE_LAYOUT_COMPONENT_BYTES), 15)
        .chain(std::iter::once("a".repeat(250)))
        .collect::<Vec<_>>()
        .join("/");
    assert_eq!("/usr/".len() + exact_path.len(), MAX_FROZEN_EXECUTABLE_PATH_BYTES);
    require_usr_relative_stone_target(&exact_path).unwrap();
    assert_eq!(
        require_usr_relative_stone_target(&format!("{exact_path}a")),
        Err("the materialized path exceeds Linux PATH_MAX")
    );
}

#[test]
fn invalid_stone_layout_batch_cannot_replace_or_insert_database_rows() {
    let layout_db = db::layout::Database::new(":memory:").unwrap();
    let retained_package = package::Id::from("retained-layout");
    let rejected_package = package::Id::from("rejected-layout");
    let retained = test_stone_layout(TestStoneLayoutKind::Regular, "bin/original");
    layout_db.add(&retained_package, &retained).unwrap();

    let replacement = test_stone_layout(TestStoneLayoutKind::Regular, "bin/replacement");
    let invalid = test_stone_layout(TestStoneLayoutKind::Directory, "/etc");
    assert!(matches!(
        ingest_stone_layouts(
            &layout_db,
            [(&retained_package, &replacement), (&rejected_package, &invalid)].into_iter(),
        ),
        Err(Error::InvalidStoneLayoutTarget {
            package,
            target,
            reason: "the target is absolute",
        }) if package == rejected_package && target == "/etc"
    ));

    assert_eq!(
        layout_db.query([&retained_package]).unwrap(),
        vec![(retained_package, retained)]
    );
    assert!(layout_db.query([&rejected_package]).unwrap().is_empty());
}

#[test]
fn reserved_stone_layout_batch_cannot_replace_or_insert_database_rows() {
    let layout_db = db::layout::Database::new(":memory:").unwrap();
    let retained_package = package::Id::from("retained-layout");
    let rejected_package = package::Id::from("reserved-layout");
    let retained = test_stone_layout(TestStoneLayoutKind::Regular, "bin/original");
    layout_db.add(&retained_package, &retained).unwrap();

    let replacement = test_stone_layout(TestStoneLayoutKind::Regular, "bin/replacement");
    let reserved = test_stone_layout(TestStoneLayoutKind::Directory, ".stateID/forged-child");
    assert!(matches!(
        ingest_stone_layouts(
            &layout_db,
            [(&retained_package, &replacement), (&rejected_package, &reserved)].into_iter(),
        ),
        Err(Error::InvalidStoneLayoutTarget {
            package,
            target,
            reason: "the target is reserved for Cast system metadata",
        }) if package == rejected_package && target == ".stateID/forged-child"
    ));

    assert_eq!(
        layout_db.query([&retained_package]).unwrap(),
        vec![(retained_package, retained)]
    );
    assert!(layout_db.query([&rejected_package]).unwrap().is_empty());
}
