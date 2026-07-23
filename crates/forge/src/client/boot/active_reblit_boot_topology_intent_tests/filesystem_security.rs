use std::{
    fs::{self, FileTimes},
    os::unix::{
        fs::{MetadataExt as _, PermissionsExt as _, symlink},
        net::UnixListener,
    },
    time::{Duration, SystemTime},
};

use super::{
    super::{
        ActiveReblitBootTopologyIntentError, BoundActiveReblitBootPartitionSelector,
        BoundActiveReblitBootTopologyIntent,
    },
    support::{
        ESP_MOUNT_POINT, ESP_PARTUUID, Fixture, TreeSnapshot, authored_alias, set_access_acl,
        set_test_xattr,
    },
};

fn lua_alias(partuuid: &str) -> String {
    format!(
        "return {{ esp = {{ partuuid = \"{partuuid}\", mount_point = \"{ESP_MOUNT_POINT}\" }}, boot = {{ kind = \"alias_esp\" }} }}\n"
    )
}

#[test]
fn missing_machine_local_intent_is_a_hard_error_without_fallback() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitBootTopologyIntentError::Missing { path }) if path == fixture.source_path()
    ));

    let missing_parent = Fixture::new();
    fs::remove_dir(missing_parent.root.join("etc/cast")).unwrap();
    assert!(matches!(
        missing_parent.prepare(),
        Err(ActiveReblitBootTopologyIntentError::Missing { path }) if path == missing_parent.source_path()
    ));
}

#[test]
fn only_registered_extensions_occupy_the_fixed_slot() {
    // An unregistered extension is never discovered.
    let fixture = Fixture::new();
    let unknown = fixture.root.join("etc/cast/boot-topology.txt");
    fs::write(&unknown, authored_alias(ESP_PARTUUID)).unwrap();
    fs::set_permissions(&unknown, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitBootTopologyIntentError::Missing { path }) if path == fixture.source_path()
    ));

    fixture.write_alias();
    fixture.prepare().unwrap();
}

#[test]
fn a_lua_source_at_the_fixed_slot_is_discovered_revalidated_and_loaded() {
    let fixture = Fixture::new();
    let lua_path = fixture.root.join("etc/cast/boot-topology.lua");
    fs::write(&lua_path, lua_alias(ESP_PARTUUID)).unwrap();
    fs::set_permissions(&lua_path, fs::Permissions::from_mode(0o644)).unwrap();

    let prepared = fixture.prepare().unwrap();
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();

    assert_eq!(
        revalidated.topology(),
        BoundActiveReblitBootTopologyIntent::BootAliasesEsp {
            esp: BoundActiveReblitBootPartitionSelector {
                partuuid: ESP_PARTUUID,
                mount_point_hint: ESP_MOUNT_POINT,
            },
        }
    );
    // The fixed slot has one canonical logical name regardless of engine, and
    // the Lua declaration imports nothing.
    let fingerprint = revalidated.fingerprint();
    assert_eq!(fingerprint.root_logical_name, "etc/cast/boot-topology.glu");
    assert!(fingerprint.modules.is_empty());
}

#[test]
fn symlink_fifo_socket_and_hardlink_sources_are_rejected_without_blocking() {
    let symlink_fixture = Fixture::new();
    let target = symlink_fixture.root.join("topology-target");
    fs::write(&target, authored_alias(ESP_PARTUUID)).unwrap();
    symlink(&target, symlink_fixture.source_path()).unwrap();
    assert!(matches!(
        symlink_fixture.prepare(),
        Err(ActiveReblitBootTopologyIntentError::UnsafeInode { .. })
    ));

    let fifo_fixture = Fixture::new();
    let fifo = std::ffi::CString::new(fifo_fixture.source_path().as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the path is NUL-terminated and names a private test location.
    assert_eq!(unsafe { nix::libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(matches!(
        fifo_fixture.prepare(),
        Err(ActiveReblitBootTopologyIntentError::UnsafeInode { .. })
    ));

    let socket_fixture = Fixture::new();
    match UnixListener::bind(socket_fixture.source_path()) {
        Ok(_listener) => assert!(matches!(
            socket_fixture.prepare(),
            Err(ActiveReblitBootTopologyIntentError::UnsafeInode { .. })
        )),
        Err(source) if source.kind() == std::io::ErrorKind::PermissionDenied => {}
        Err(source) => panic!("bind private topology test socket: {source}"),
    }

    let hardlink_fixture = Fixture::new();
    hardlink_fixture.write_alias();
    fs::hard_link(
        hardlink_fixture.source_path(),
        hardlink_fixture.root.join("second-name"),
    )
    .unwrap();
    assert!(matches!(
        hardlink_fixture.prepare(),
        Err(ActiveReblitBootTopologyIntentError::UnsafeInode { .. })
    ));
}

#[test]
fn unsafe_source_and_ancestor_modes_fail_closed() {
    for mode in [0o666, 0o744, 0o4644] {
        let fixture = Fixture::new();
        fixture.write_alias();
        fs::set_permissions(fixture.source_path(), fs::Permissions::from_mode(mode)).unwrap();
        assert!(matches!(
            fixture.prepare(),
            Err(ActiveReblitBootTopologyIntentError::UnsafeInode { .. })
        ));
    }

    for (relative, mode) in [("etc", 0o775), ("etc/cast", 0o777), ("etc/cast", 0o1755)] {
        let fixture = Fixture::new();
        fixture.write_alias();
        fs::set_permissions(fixture.root.join(relative), fs::Permissions::from_mode(mode)).unwrap();
        assert!(matches!(
            fixture.prepare(),
            Err(ActiveReblitBootTopologyIntentError::UnsafeInode { .. })
        ));
    }
}

#[test]
fn source_and_ancestor_acls_or_xattrs_are_rejected_when_supported() {
    for target in ["source", "directory"] {
        let xattr_fixture = Fixture::new();
        xattr_fixture.write_alias();
        let path = if target == "source" {
            xattr_fixture.source_path()
        } else {
            xattr_fixture.root.join("etc/cast")
        };
        if set_test_xattr(&path).unwrap() {
            assert!(matches!(
                xattr_fixture.prepare(),
                Err(ActiveReblitBootTopologyIntentError::Io { .. })
            ));
        }

        let acl_fixture = Fixture::new();
        acl_fixture.write_alias();
        let path = if target == "source" {
            acl_fixture.source_path()
        } else {
            acl_fixture.root.join("etc/cast")
        };
        if set_access_acl(&path).unwrap() {
            assert!(matches!(
                acl_fixture.prepare(),
                Err(ActiveReblitBootTopologyIntentError::Io { .. })
                    | Err(ActiveReblitBootTopologyIntentError::UnsafeInode { .. })
            ));
        }
    }
}

#[test]
fn invalid_intent_and_all_structural_failures_have_zero_mutation() {
    let fixture = Fixture::new();
    fixture.write_source("not valid Gluon");
    let before = TreeSnapshot::capture(&fixture.root);
    assert!(fixture.prepare().is_err());
    assert_eq!(TreeSnapshot::capture(&fixture.root), before);

    let unsafe_fixture = Fixture::new();
    unsafe_fixture.write_alias();
    fs::set_permissions(unsafe_fixture.source_path(), fs::Permissions::from_mode(0o666)).unwrap();
    let before = TreeSnapshot::capture(&unsafe_fixture.root);
    assert!(unsafe_fixture.prepare().is_err());
    assert_eq!(TreeSnapshot::capture(&unsafe_fixture.root), before);
}

#[test]
fn descriptor_reads_preserve_source_and_directory_atime() {
    let fixture = Fixture::new();
    fixture.write_alias();
    let source = fixture.source_path();
    let directory = fixture.root.join("etc/cast");
    let times = FileTimes::new()
        .set_accessed(SystemTime::UNIX_EPOCH + Duration::from_secs(100))
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(200));
    fs::File::open(&source).unwrap().set_times(times).unwrap();
    fs::File::open(&directory).unwrap().set_times(times).unwrap();
    let source_atime = atime(&source);
    let directory_atime = atime(&directory);

    let prepared = fixture.prepare().unwrap();
    prepared.revalidate(&fixture.installation).unwrap();
    assert_eq!(atime(&source), source_atime);
    assert_eq!(atime(&directory), directory_atime);
}

fn atime(path: &std::path::Path) -> (i64, i64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.atime(), metadata.atime_nsec())
}
