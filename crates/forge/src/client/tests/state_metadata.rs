fn state_metadata_mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn install_state_metadata_test_default_acl(path: &Path) -> io::Result<()> {
    const ACL: [u8; 28] = [
        0x02, 0x00, 0x00, 0x00, // version
        0x01, 0x00, 0x07, 0x00, 0xff, 0xff, 0xff, 0xff, // user object
        0x04, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // group object
        0x20, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // other
    ];
    let directory = std::fs::File::open(path)?;
    // SAFETY: the descriptor, static name, and complete canonical ACL
    // encoding remain live for the syscall.
    let result = unsafe {
        nix::libc::fsetxattr(
            directory.as_raw_fd(),
            c"system.posix_acl_default".as_ptr(),
            ACL.as_ptr().cast(),
            ACL.len(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[test]
fn state_metadata_rejects_inheritable_default_acls() {
    let temporary = tempfile::tempdir().unwrap();
    fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();
    match install_state_metadata_test_default_acl(temporary.path()) {
        Ok(()) => {}
        Err(source) if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) => return,
        Err(source) => panic!("install state metadata test default ACL: {source}"),
    }

    let inherited_root = temporary.path().join("inherited-root");
    assert!(record_state_id(&inherited_root, state::Id::from(40)).is_err());
    assert!(inherited_root.is_dir());
    assert!(!inherited_root.join("usr").exists());

    let isolated = tempfile::tempdir().unwrap();
    fs::set_permissions(isolated.path(), Permissions::from_mode(0o700)).unwrap();
    let root = isolated.path().join("root");
    let usr = root.join("usr");
    fs::create_dir_all(&usr).unwrap();
    fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();
    install_state_metadata_test_default_acl(&usr).unwrap();
    assert!(record_state_id(&root, state::Id::from(41)).is_err());
    assert!(!usr.join(".stateID").exists());
}

#[test]
fn state_metadata_creation_has_exact_modes_under_hostile_umasks() {
    const CHILD: &str = "CAST_STATE_METADATA_UMASK_TEST_CHILD";
    const ROOT: &str = "CAST_STATE_METADATA_UMASK_TEST_ROOT";
    const TEST: &str = "client::tests::state_metadata_creation_has_exact_modes_under_hostile_umasks";

    if let Some(mask) = std::env::var_os(CHILD) {
        let mask = u32::from_str_radix(mask.to_str().unwrap(), 8).unwrap();
        // umask is process-global. This child runs one exact test and
        // exits immediately after exercising the selected mask.
        // SAFETY: no other test runs in this single-test child process.
        unsafe { nix::libc::umask(mask) };
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        record_state_id(&root, state::Id::from(42)).unwrap();
        assert_eq!(state_metadata_mode(&root), STATE_TREE_DIRECTORY_MODE);
        assert_eq!(state_metadata_mode(&root.join("usr")), STATE_TREE_DIRECTORY_MODE);
        assert_eq!(state_metadata_mode(&root.join("usr/.stateID")), STATE_ID_MODE);
        assert_eq!(fs::read_to_string(root.join("usr/.stateID")).unwrap(), "42");
        return;
    }

    for mask in ["0002", "0777"] {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();
        let root = temporary.path().join("state-root");
        let output = Command::new(std::env::current_exe().unwrap())
            .arg(TEST)
            .arg("--exact")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(CHILD, mask)
            .env(ROOT, &root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "state metadata umask {mask} child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn state_metadata_recovers_restrictive_directory_creation_residue() {
    let temporary = tempfile::tempdir().unwrap();
    fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();

    let root_residue = temporary.path().join("root-residue");
    fs::create_dir(&root_residue).unwrap();
    fs::set_permissions(&root_residue, Permissions::from_mode(0o000)).unwrap();
    record_state_id(&root_residue, state::Id::from(43)).unwrap();
    assert_eq!(state_metadata_mode(&root_residue), STATE_TREE_DIRECTORY_MODE);
    assert_eq!(
        state_metadata_mode(&root_residue.join("usr")),
        STATE_TREE_DIRECTORY_MODE
    );

    let root = temporary.path().join("root");
    let usr_residue = root.join("usr");
    fs::create_dir_all(&usr_residue).unwrap();
    fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&usr_residue, Permissions::from_mode(0o400)).unwrap();
    record_state_id(&root, state::Id::from(44)).unwrap();
    assert_eq!(state_metadata_mode(&root), 0o700);
    assert_eq!(state_metadata_mode(&usr_residue), STATE_TREE_DIRECTORY_MODE);
    assert_eq!(fs::read_to_string(usr_residue.join(STATE_ID_NAME)).unwrap(), "44");
}

#[test]
fn state_metadata_recovers_private_atomic_temporary_residue() {
    for (bytes, mode) in [
        (b"".as_slice(), 0o000),
        (b"4".as_slice(), 0o400),
        (b"wrong".as_slice(), 0o600),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let usr = root.join("usr");
        fs::create_dir_all(&usr).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();
        let residue = usr.join(STATE_ID_TEMPORARY_NAME);
        fs::write(&residue, bytes).unwrap();
        fs::set_permissions(&residue, Permissions::from_mode(mode)).unwrap();

        record_state_id(&root, state::Id::from(45)).unwrap();

        assert_eq!(fs::read_to_string(usr.join(STATE_ID_NAME)).unwrap(), "45");
        assert_eq!(state_metadata_mode(&usr.join(STATE_ID_NAME)), STATE_ID_MODE);
        assert!(!residue.exists());
    }
}

#[test]
fn unsafe_state_metadata_temporary_is_rejected_unchanged() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let usr = root.join("usr");
    fs::create_dir_all(&usr).unwrap();
    fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();
    let target = temporary.path().join("external");
    fs::write(&target, b"external evidence").unwrap();
    let residue = usr.join(STATE_ID_TEMPORARY_NAME);
    symlink(&target, &residue).unwrap();

    assert!(record_state_id(&root, state::Id::from(46)).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"external evidence");
    assert!(fs::symlink_metadata(&residue).unwrap().file_type().is_symlink());
    assert!(!usr.join(STATE_ID_NAME).exists());

    fs::remove_file(&residue).unwrap();
    fs::write(&residue, b"linked evidence").unwrap();
    fs::set_permissions(&residue, Permissions::from_mode(STATE_ID_TEMPORARY_MODE)).unwrap();
    let second = usr.join("state-id-temporary-second-link");
    fs::hard_link(&residue, &second).unwrap();
    assert!(record_state_id(&root, state::Id::from(47)).is_err());
    assert_eq!(fs::read(&residue).unwrap(), b"linked evidence");
    assert_eq!(fs::metadata(&residue).unwrap().nlink(), 2);
    assert!(!usr.join(STATE_ID_NAME).exists());
}

#[test]
fn state_metadata_rejects_symlink_and_non_directory_components_unchanged() {
    let temporary = tempfile::tempdir().unwrap();

    let real_root = temporary.path().join("real-root");
    fs::create_dir(&real_root).unwrap();
    fs::set_permissions(&real_root, Permissions::from_mode(0o700)).unwrap();
    let root_alias = temporary.path().join("root-alias");
    symlink(&real_root, &root_alias).unwrap();
    assert!(record_state_id(&root_alias, state::Id::from(1)).is_err());
    assert!(!real_root.join("usr").exists());

    let file_root = temporary.path().join("file-root");
    fs::write(&file_root, b"root evidence").unwrap();
    assert!(record_state_id(&file_root, state::Id::from(2)).is_err());
    assert_eq!(fs::read(&file_root).unwrap(), b"root evidence");

    let root = temporary.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
    let redirected_usr = temporary.path().join("redirected-usr");
    fs::create_dir(&redirected_usr).unwrap();
    let usr_alias = root.join("usr");
    symlink(&redirected_usr, &usr_alias).unwrap();
    assert!(record_state_id(&root, state::Id::from(3)).is_err());
    assert!(!redirected_usr.join(".stateID").exists());
    assert!(fs::symlink_metadata(&usr_alias).unwrap().file_type().is_symlink());

    fs::remove_file(&usr_alias).unwrap();
    fs::write(&usr_alias, b"usr evidence").unwrap();
    assert!(record_state_id(&root, state::Id::from(4)).is_err());
    assert_eq!(fs::read(&usr_alias).unwrap(), b"usr evidence");
}

#[test]
fn state_metadata_rejects_non_regular_or_linked_markers_unchanged() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let usr = root.join("usr");
    fs::create_dir_all(&usr).unwrap();
    fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();

    let external = temporary.path().join("external");
    fs::write(&external, b"external evidence").unwrap();
    let marker = usr.join(".stateID");
    symlink(&external, &marker).unwrap();
    assert!(record_state_id(&root, state::Id::from(5)).is_err());
    assert_eq!(fs::read(&external).unwrap(), b"external evidence");
    assert!(fs::symlink_metadata(&marker).unwrap().file_type().is_symlink());

    fs::remove_file(&marker).unwrap();
    fs::create_dir(&marker).unwrap();
    assert!(record_state_id(&root, state::Id::from(6)).is_err());
    assert!(marker.is_dir());

    fs::remove_dir(&marker).unwrap();
    fs::write(&marker, b"linked evidence").unwrap();
    fs::set_permissions(&marker, Permissions::from_mode(STATE_ID_MODE)).unwrap();
    let second_link = usr.join("state-id-second-link");
    fs::hard_link(&marker, &second_link).unwrap();
    assert!(record_state_id(&root, state::Id::from(7)).is_err());
    assert_eq!(fs::read(&marker).unwrap(), b"linked evidence");
    assert_eq!(fs::read(&second_link).unwrap(), b"linked evidence");

    fs::remove_file(&marker).unwrap();
    fs::remove_file(&second_link).unwrap();
    fs::write(&marker, b"").unwrap();
    fs::set_permissions(&marker, Permissions::from_mode(0o000)).unwrap();
    record_state_id(&root, state::Id::from(8)).unwrap();
    assert_eq!(state_metadata_mode(&marker), STATE_ID_MODE);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "8");
}

#[test]
fn state_metadata_preserves_safe_existing_directory_modes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let usr = root.join("usr");
    fs::create_dir_all(&usr).unwrap();
    fs::set_permissions(&root, Permissions::from_mode(0o711)).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();

    record_state_id(&root, state::Id::from(9)).unwrap();
    let first_inode = fs::metadata(usr.join(STATE_ID_NAME)).unwrap().ino();
    record_state_id(&root, state::Id::from(10)).unwrap();

    assert_eq!(state_metadata_mode(&root), 0o711);
    assert_eq!(state_metadata_mode(&usr), 0o750);
    assert_eq!(state_metadata_mode(&usr.join(".stateID")), STATE_ID_MODE);
    assert_eq!(fs::read_to_string(usr.join(".stateID")).unwrap(), "10");
    assert_ne!(fs::metadata(usr.join(STATE_ID_NAME)).unwrap().ino(), first_inode);
    assert!(!usr.join(STATE_ID_TEMPORARY_NAME).exists());
}
