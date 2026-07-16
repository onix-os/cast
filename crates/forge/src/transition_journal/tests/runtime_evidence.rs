#[test]
fn runtime_epoch_capture_is_canonical_stable_and_current() {
    let first = RuntimeEpoch::capture().unwrap();
    let second = RuntimeEpoch::capture().unwrap();

    assert_eq!(first, second);
    assert_eq!(first.boot_id.as_str().len(), BootId::TEXT_LENGTH);
    assert_ne!(first.mount_namespace.st_dev, 0);
    assert_ne!(first.mount_namespace.inode, 0);
}

#[test]
fn runtime_tree_identity_capture_binds_the_exact_directory_and_mount() {
    let temporary = tempfile::tempdir().unwrap();
    let root = fs::File::open(temporary.path()).unwrap();
    fs::create_dir(temporary.path().join("child")).unwrap();
    let child = fs::File::open(temporary.path().join("child")).unwrap();

    let root_identity = RuntimeTreeIdentity::capture_directory(&root).unwrap();
    let repeated = RuntimeTreeIdentity::capture_directory(&root).unwrap();
    let child_identity = RuntimeTreeIdentity::capture_directory(&child).unwrap();

    assert_eq!(root_identity, repeated);
    assert_eq!(root_identity.st_dev, child_identity.st_dev);
    assert_eq!(root_identity.mount_id, child_identity.mount_id);
    assert_ne!(root_identity.inode, child_identity.inode);
}

#[test]
fn runtime_tree_identity_rejects_a_non_directory_descriptor() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("regular");
    fs::write(&path, b"regular").unwrap();
    let file = fs::File::open(path).unwrap();

    assert!(matches!(
        RuntimeTreeIdentity::capture_directory(&file),
        Err(RuntimeEvidenceError::TreeIsNotDirectory)
    ));
}

#[test]
fn boot_id_and_mount_namespace_parsers_reject_untrusted_or_noncanonical_inputs() {
    let canonical = b"01234567-89ab-4cde-8f01-23456789abcd\n";
    assert_eq!(
        runtime_evidence::parse_boot_id_bytes(canonical).unwrap().as_str(),
        "01234567-89ab-4cde-8f01-23456789abcd"
    );
    for invalid in [
        b"01234567-89ab-4cde-8f01-23456789abcd".as_slice(),
        b"01234567-89ab-4cde-8f01-23456789abcd\nextra",
        b"00000000-0000-0000-0000-000000000000\n",
        b"01234567-89ab-4cde-8f01-23456789abcg\n",
    ] {
        assert!(runtime_evidence::parse_boot_id_bytes(invalid).is_err());
    }

    let temporary = tempfile::tempdir().unwrap();
    let ordinary = fs::File::open(temporary.path()).unwrap();
    assert!(matches!(
        runtime_evidence::mount_namespace_identity(&ordinary),
        Err(RuntimeEvidenceError::AuthenticateMountNamespace(_))
    ));
}

#[test]
fn fdinfo_mount_id_parser_is_bounded_canonical_and_unique() {
    assert_eq!(
        crate::linux_fs::parse_descriptor_mount_id(b"pos:\t0\nflags:\t0100000\nmnt_id:\t42\nino:\t9\n").unwrap(),
        42
    );
    for invalid in [
        b"pos:\t0\n".as_slice(),
        b"mnt_id: 42\n",
        b"mnt_id:\t0\n",
        b"mnt_id:\t042\n",
        b"mnt_id:\t18446744073709551616\n",
        b"mnt_id:\t42\nmnt_id:\t42\n",
        b"mnt_id:\t42",
        b"mnt_id:\t42\0\n",
    ] {
        assert_eq!(
            crate::linux_fs::parse_descriptor_mount_id(invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    assert_eq!(
        crate::linux_fs::parse_descriptor_mount_id(&vec![b'x'; 16 * 1024 + 1])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}
