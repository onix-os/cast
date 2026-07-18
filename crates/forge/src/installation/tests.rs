use std::{
    os::unix::{
        ffi::OsStrExt as _,
        fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _, symlink},
    },
    process::Command,
};

use fs_err as fs;

use crate::test_support::private_installation_tempdir;

use super::*;

#[test]
fn open_defers_canonical_authored_system_intent_to_the_client_gate() {
    let temporary = private_installation_tempdir();
    let path = system_model::intent_path(temporary.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let authored = r#"let cast = import! cast.system.v1
{
    packages = ["alpha"],
    .. cast.system
}
"#;
    fs::write(&path, authored).unwrap();

    let installation = Installation::open(temporary.path(), None).unwrap();
    assert_eq!(installation.system_intent_path(), path);
    assert!(installation.system_model.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), authored);
    assert!(!system_model::snapshot_path(temporary.path()).exists());
}

#[test]
fn both_open_modes_defer_invalid_system_intent_but_frozen_skips_active_state() {
    let temporary = private_installation_tempdir();
    let intent_path = system_model::intent_path(temporary.path());
    fs::create_dir_all(intent_path.parent().unwrap()).unwrap();
    fs::write(&intent_path, b"invalid Gluon that normal open must reject").unwrap();
    fs::create_dir_all(temporary.path().join("usr")).unwrap();
    let state_id = temporary.path().join("usr/.stateID");
    fs::write(&state_id, b"73").unwrap();
    fs::set_permissions(&state_id, std::fs::Permissions::from_mode(0o644)).unwrap();

    let frozen = Installation::open_frozen(temporary.path(), None).unwrap();
    assert!(frozen.active_state.is_none());
    assert!(frozen.system_model.is_none());
    assert!(frozen.is_frozen_cache());
    drop(frozen);

    let system = Installation::open(temporary.path(), None).unwrap();
    assert_eq!(system.active_state, Some(state::Id::from(73)));
    assert!(system.system_model.is_none());
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn install_test_default_acl(path: &Path) -> io::Result<()> {
    // Linux POSIX ACL xattr encoding: version 2 followed by the canonical
    // user::rwx, group::r-x, and other::r-x default entries. A default ACL
    // does not appear in this directory's st_mode but would be inherited
    // by later children if installation provisioning admitted it.
    const ACL: [u8; 28] = [
        0x02, 0x00, 0x00, 0x00, // version
        0x01, 0x00, 0x07, 0x00, 0xff, 0xff, 0xff, 0xff, // user object
        0x04, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // group object
        0x20, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // other
    ];
    let directory = std::fs::File::open(path)?;
    // SAFETY: the directory, static name, and complete ACL byte array
    // remain live for the syscall.
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

fn prepare_cast_parent(root: &Path) -> PathBuf {
    let cast = root.join(".cast");
    std::fs::create_dir(&cast).unwrap();
    std::fs::set_permissions(&cast, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
    cast
}

fn prepare_cache_parent(root: &Path) -> PathBuf {
    let cast = prepare_cast_parent(root);
    let cache = cast.join("cache");
    std::fs::create_dir(&cache).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
    cache
}

#[test]
fn newly_created_capability_roots_have_exact_private_mode() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();

    assert_eq!(mode(&temporary.path().join(".cast")), PRIVATE_DIRECTORY_MODE);
    assert_eq!(mode(&installation.cache_path("")), PRIVATE_DIRECTORY_MODE);
    assert_eq!(mode(&installation.assets_path("")), PRIVATE_DIRECTORY_MODE);
    assert_eq!(mode(&installation.state_quarantine_dir()), PRIVATE_DIRECTORY_MODE);
    assert_eq!(mode(&temporary.path().join(".cast/.cast-lockfile")), LOCKFILE_MODE);
    let tag = installation.cache_path("CACHEDIR.TAG");
    assert_eq!(mode(&tag), CACHEDIR_TAG_MODE);
    assert_eq!(std::fs::read(tag).unwrap(), CACHEDIR_TAG_CONTENTS);
}

#[test]
fn safe_0555_installation_root_opens_read_only_without_provisioning() {
    let temporary = private_installation_tempdir();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

    let installation = Installation::open_frozen(temporary.path(), None).unwrap();

    assert!(installation.read_only());
    assert!(!temporary.path().join(".cast").exists());
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn installation_root_owner_and_write_policy_is_explicit() {
    let path = Path::new("/policy-test");
    assert!(require_installation_root_policy(true, 0, 0o555, 1000, path).is_ok());
    assert!(require_installation_root_policy(true, 1000, 0o755, 1000, path).is_ok());
    assert_eq!(
        require_installation_root_policy(true, 1001, 0o555, 1000, path)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(classify_installation_root_access(0, 0o755, 1000), Mutability::ReadOnly);
    assert_eq!(
        classify_installation_root_access(1000, 0o555, 1000),
        Mutability::ReadOnly
    );
    assert_eq!(
        classify_installation_root_access(1000, 0o755, 1000),
        Mutability::ReadWrite
    );
}

#[test]
fn installation_root_default_acl_is_rejected_before_provisioning() {
    let temporary = private_installation_tempdir();
    match install_test_default_acl(temporary.path()) {
        Ok(()) => {}
        Err(source) if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) => return,
        Err(source) => panic!("install test default ACL: {source}"),
    }
    assert_eq!(mode(temporary.path()), PRIVATE_DIRECTORY_MODE);

    let error = Installation::open(temporary.path(), None).unwrap_err();
    assert!(matches!(
        error,
        Error::ValidateRootDirectory { path, source }
            if path == temporary.path() && source.kind() == io::ErrorKind::PermissionDenied
    ));
    assert!(!temporary.path().join(".cast").exists());
}

#[test]
fn existing_capability_default_acl_is_rejected_without_creating_children() {
    let temporary = private_installation_tempdir();
    let cast = prepare_cast_parent(temporary.path());
    match install_test_default_acl(&cast) {
        Ok(()) => {}
        Err(source) if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) => return,
        Err(source) => panic!("install test default ACL: {source}"),
    }

    let error = Installation::open(temporary.path(), None).unwrap_err();
    assert!(matches!(
        error,
        Error::PrepareDirectory { path, source }
            if path == cast && source.kind() == io::ErrorKind::PermissionDenied
    ));
    assert!(!cast.join("cache").exists());
}

#[test]
fn named_installation_root_revalidation_obeys_deadline_and_detects_substitution() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().join("root");
    let detached = temporary.path().join("detached-root");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
    let installation = Installation::open(&root, None).unwrap();

    installation
        .revalidate_root_directory_until(Instant::now() + std::time::Duration::from_secs(30))
        .unwrap();
    let error = installation
        .revalidate_root_directory_until(Instant::now() - std::time::Duration::from_millis(1))
        .unwrap_err();
    assert!(matches!(
        error,
        Error::ValidateRootDirectory { path, source }
            if path == root && source.kind() == io::ErrorKind::TimedOut
    ));

    std::fs::rename(&root, &detached).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();

    let error = installation
        .revalidate_root_directory_until(Instant::now() + std::time::Duration::from_secs(30))
        .unwrap_err();
    assert!(matches!(
        error,
        Error::ValidateRootDirectory { path, source }
            if path == root && source.kind() != io::ErrorKind::TimedOut
    ));
    assert!(installation.revalidate_root_directory().is_err());
    assert_ne!(
        std::fs::metadata(&root).unwrap().ino(),
        std::fs::metadata(&detached).unwrap().ino()
    );
}

#[test]
fn cachedir_tag_recovers_only_through_private_atomic_temporaries() {
    for (prefix, residue_mode) in [(0, 0o000), (17, 0o400), (CACHEDIR_TAG_CONTENTS.len() / 2, 0o600)] {
        let temporary = private_installation_tempdir();
        let cache = prepare_cache_parent(temporary.path());
        let residue = cache.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
        std::fs::write(&residue, &CACHEDIR_TAG_CONTENTS[..prefix]).unwrap();
        std::fs::set_permissions(&residue, std::fs::Permissions::from_mode(residue_mode)).unwrap();

        let installation = Installation::open(temporary.path(), None).unwrap();

        let canonical = installation.cache_path("CACHEDIR.TAG");
        assert_eq!(std::fs::read(canonical).unwrap(), CACHEDIR_TAG_CONTENTS);
        assert_eq!(mode(&installation.cache_path("CACHEDIR.TAG")), CACHEDIR_TAG_MODE);
        assert!(!residue.exists());
    }
}

#[test]
fn complete_fsynced_cachedir_temporary_is_published_without_rewriting() {
    let temporary = private_installation_tempdir();
    let cache = prepare_cache_parent(temporary.path());
    let residue = cache.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
    std::fs::write(&residue, CACHEDIR_TAG_CONTENTS).unwrap();
    std::fs::set_permissions(&residue, std::fs::Permissions::from_mode(CACHEDIR_TAG_MODE)).unwrap();
    std::fs::File::open(&residue).unwrap().sync_all().unwrap();
    let inode = std::fs::metadata(&residue).unwrap().ino();

    let installation = Installation::open(temporary.path(), None).unwrap();
    let canonical = installation.cache_path("CACHEDIR.TAG");

    assert_eq!(std::fs::metadata(&canonical).unwrap().ino(), inode);
    assert_eq!(std::fs::read(canonical).unwrap(), CACHEDIR_TAG_CONTENTS);
    assert!(!residue.exists());
}

#[test]
fn corrupt_canonical_cachedir_tags_fail_unchanged() {
    for contents in [
        b"not a cache tag".to_vec(),
        vec![b'x'; CACHEDIR_TAG_CONTENTS.len()],
        [CACHEDIR_TAG_CONTENTS, b"extra"].concat(),
    ] {
        let temporary = private_installation_tempdir();
        let cache = prepare_cache_parent(temporary.path());
        let canonical = cache.join("CACHEDIR.TAG");
        std::fs::write(&canonical, &contents).unwrap();
        std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(CACHEDIR_TAG_MODE)).unwrap();
        let original = std::fs::read(&canonical).unwrap();

        assert!(matches!(
            Installation::open(temporary.path(), None),
            Err(Error::PrepareCachedirTag { path, .. }) if path == canonical
        ));
        assert_eq!(std::fs::read(&canonical).unwrap(), original);
        assert!(
            !cache
                .join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref())
                .exists()
        );
    }
}

#[test]
fn special_canonical_cachedir_tags_fail_before_data_open_and_remain_unchanged() {
    for kind in ["fifo", "symlink", "directory"] {
        let temporary = private_installation_tempdir();
        let cache = prepare_cache_parent(temporary.path());
        let canonical = cache.join("CACHEDIR.TAG");
        match kind {
            "fifo" => {
                let encoded = CString::new(canonical.as_os_str().as_bytes()).unwrap();
                // SAFETY: the path is NUL-terminated and names a missing
                // entry inside the private test directory.
                assert_eq!(unsafe { nix::libc::mkfifo(encoded.as_ptr(), CACHEDIR_TAG_MODE) }, 0);
                std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(CACHEDIR_TAG_MODE)).unwrap();
            }
            "symlink" => {
                std::fs::write(cache.join("target"), CACHEDIR_TAG_CONTENTS).unwrap();
                symlink("target", &canonical).unwrap();
            }
            "directory" => {
                std::fs::create_dir(&canonical).unwrap();
            }
            _ => unreachable!(),
        }
        let before = std::fs::symlink_metadata(&canonical).unwrap();

        assert!(matches!(
            Installation::open(temporary.path(), None),
            Err(Error::PrepareCachedirTag { path, .. }) if path == canonical
        ));

        let after = std::fs::symlink_metadata(&canonical).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()), "{kind}");
        assert_eq!(after.file_type().is_fifo(), before.file_type().is_fifo(), "{kind}");
        assert_eq!(
            after.file_type().is_symlink(),
            before.file_type().is_symlink(),
            "{kind}"
        );
        assert_eq!(after.file_type().is_dir(), before.file_type().is_dir(), "{kind}");
        if kind == "symlink" {
            assert_eq!(std::fs::read_link(&canonical).unwrap(), Path::new("target"));
        }
        assert!(
            !cache
                .join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref())
                .exists()
        );
    }
}

#[test]
fn unsafe_cachedir_temporary_evidence_is_never_repaired_or_removed() {
    let temporary = private_installation_tempdir();
    let cache = prepare_cache_parent(temporary.path());
    let residue = cache.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
    std::fs::write(&residue, b"partial").unwrap();
    std::fs::set_permissions(&residue, std::fs::Permissions::from_mode(0o660)).unwrap();
    let original = std::fs::read(&residue).unwrap();

    assert!(matches!(
        Installation::open(temporary.path(), None),
        Err(Error::PrepareCachedirTag { .. })
    ));
    assert_eq!(mode(&residue), 0o660);
    assert_eq!(std::fs::read(&residue).unwrap(), original);
    assert!(!cache.join("CACHEDIR.TAG").exists());
}

#[test]
fn hardlinked_cachedir_temporary_evidence_fails_unchanged() {
    let temporary = private_installation_tempdir();
    let cache = prepare_cache_parent(temporary.path());
    let residue = cache.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
    let second = cache.join("residue-link");
    std::fs::write(&residue, b"partial").unwrap();
    std::fs::set_permissions(&residue, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&residue, &second).unwrap();

    assert!(matches!(
        Installation::open(temporary.path(), None),
        Err(Error::PrepareCachedirTag { .. })
    ));
    assert_eq!(std::fs::metadata(&residue).unwrap().nlink(), 2);
    assert_eq!(std::fs::read(&residue).unwrap(), b"partial");
    assert_eq!(std::fs::read(&second).unwrap(), b"partial");
    assert!(!cache.join("CACHEDIR.TAG").exists());
}

#[test]
fn umask_0777_cannot_strand_new_capability_roots() {
    const CHILD: &str = "CAST_INSTALLATION_UMASK_TEST_CHILD";
    const TEST: &str = "installation::tests::umask_0777_cannot_strand_new_capability_roots";

    if let Some(root) = std::env::var_os(CHILD) {
        // umask is process-global, so mutate it only in the isolated test
        // process selected by the parent branch below.
        // SAFETY: this child runs one exact test and exits immediately.
        unsafe { nix::libc::umask(0o777) };
        let installation = Installation::open(PathBuf::from(root), None).unwrap();
        assert_eq!(mode(&installation.root.join(".cast")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.cache_path("")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.assets_path("")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.state_quarantine_dir()), PRIVATE_DIRECTORY_MODE);
        for path in [
            installation.root.join(".cast/db"),
            installation.root.join(".cast/repo"),
            installation.root.join(".cast/root"),
            installation.root.join(".cast/root/staging"),
            installation.root.join(".cast/root/isolation"),
        ] {
            assert_eq!(mode(&path), PRIVATE_DIRECTORY_MODE, "{}", path.display());
        }
        return;
    }

    let temporary = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        temporary.path(),
        std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
    )
    .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg(TEST)
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD, temporary.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "hostile-umask child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn restrictive_owner_only_crash_residue_is_recovered_exactly() {
    let temporary = private_installation_tempdir();
    let cast = temporary.path().join(".cast");
    let cache = cast.join("cache");
    let assets = cast.join("assets");
    let quarantine = cast.join("quarantine");
    for directory in [&cast, &cache, &assets, &quarantine] {
        std::fs::create_dir(directory).unwrap();
    }
    for (directory, residue) in [(&cache, 0o000), (&assets, 0o400), (&quarantine, 0o500), (&cast, 0o600)] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(residue)).unwrap();
    }

    let installation = Installation::open(temporary.path(), None).unwrap();

    assert_eq!(mode(&installation.root.join(".cast")), PRIVATE_DIRECTORY_MODE);
    assert_eq!(mode(&installation.cache_path("")), PRIVATE_DIRECTORY_MODE);
    assert_eq!(mode(&installation.assets_path("")), PRIVATE_DIRECTORY_MODE);
    assert_eq!(mode(&installation.state_quarantine_dir()), PRIVATE_DIRECTORY_MODE);
}

#[test]
fn unsafe_preexisting_cast_root_is_unchanged_and_blocks_children() {
    let temporary = private_installation_tempdir();
    let cast = temporary.path().join(".cast");
    std::fs::create_dir(&cast).unwrap();
    std::fs::set_permissions(&cast, std::fs::Permissions::from_mode(0o770)).unwrap();

    let error = Installation::open(temporary.path(), None).unwrap_err();
    assert!(matches!(
        error,
        Error::PrepareDirectory { path, source }
            if path == cast && source.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(mode(&cast), 0o770);
    assert!(!cast.join("cache").exists());
}

#[test]
fn installation_lockfile_symlink_is_rejected_without_touching_target() {
    let temporary = private_installation_tempdir();
    let cast = prepare_cast_parent(temporary.path());
    let target = temporary.path().join("external-lock-target");
    std::fs::write(&target, b"evidence").unwrap();
    let lockfile = cast.join(".cast-lockfile");
    symlink(&target, &lockfile).unwrap();

    let error = Installation::open(temporary.path(), None).unwrap_err();
    assert!(matches!(error, Error::PrepareLockfile { path, .. } if path == lockfile));
    assert_eq!(std::fs::read(&target).unwrap(), b"evidence");
    assert!(std::fs::symlink_metadata(&lockfile).unwrap().file_type().is_symlink());
}

#[test]
fn installation_lockfile_requires_one_safe_inode_and_recovers_private_residue() {
    let temporary = private_installation_tempdir();
    let cast = prepare_cast_parent(temporary.path());
    let lockfile = cast.join(".cast-lockfile");
    let second = cast.join("second-lock-link");
    std::fs::write(&lockfile, b"").unwrap();
    std::fs::set_permissions(&lockfile, std::fs::Permissions::from_mode(LOCKFILE_MODE)).unwrap();
    std::fs::hard_link(&lockfile, &second).unwrap();

    let error = Installation::open(temporary.path(), None).unwrap_err();
    assert!(matches!(error, Error::PrepareLockfile { path, .. } if path == lockfile));
    assert_eq!(std::fs::metadata(&lockfile).unwrap().nlink(), 2);

    std::fs::remove_file(&second).unwrap();
    std::fs::set_permissions(&lockfile, std::fs::Permissions::from_mode(0o000)).unwrap();
    let installation = Installation::open(temporary.path(), None).unwrap();
    assert_eq!(mode(&lockfile), LOCKFILE_MODE);
    drop(installation);

    std::fs::set_permissions(&lockfile, std::fs::Permissions::from_mode(0o644)).unwrap();
    let installation = Installation::open(temporary.path(), None).unwrap();
    assert_eq!(mode(&lockfile), 0o644);
    drop(installation);
}

#[test]
fn unsafe_installation_root_is_rejected_before_cast_creation() {
    for unsafe_mode in [0o775, 0o777] {
        let temporary = private_installation_tempdir();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(unsafe_mode)).unwrap();

        let error = Installation::open(temporary.path(), None).unwrap_err();
        assert!(matches!(
            error,
            Error::ValidateRootDirectory { path, source }
                if path == temporary.path() && source.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(mode(temporary.path()), unsafe_mode);
        assert!(!temporary.path().join(".cast").exists());
    }
}

#[test]
fn existing_group_writable_cache_root_is_rejected_without_chmod_laundering() {
    let temporary = private_installation_tempdir();
    let cast = prepare_cast_parent(temporary.path());
    let cache = cast.join("cache");
    std::fs::create_dir(&cache).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o775)).unwrap();

    let error = Installation::open(temporary.path(), None).unwrap_err();
    assert!(matches!(
        error,
        Error::PrepareDirectory { path, source }
            if path == cache && source.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(mode(&cache), 0o775);
}

#[test]
fn existing_group_writable_state_quarantine_is_rejected_without_chmod_laundering() {
    let temporary = private_installation_tempdir();
    let cast = prepare_cast_parent(temporary.path());
    for directory in ["cache", "assets"] {
        let path = cast.join(directory);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
    }
    let quarantine = cast.join("quarantine");
    std::fs::create_dir(&quarantine).unwrap();
    std::fs::set_permissions(&quarantine, std::fs::Permissions::from_mode(0o775)).unwrap();

    let error = Installation::open(temporary.path(), None).unwrap_err();
    assert!(matches!(
        error,
        Error::PrepareDirectory { path, source }
            if path == quarantine && source.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(mode(&quarantine), 0o775);
}

#[test]
fn existing_readonly_shared_cache_root_remains_compatible() {
    let temporary = private_installation_tempdir();
    let cast = prepare_cast_parent(temporary.path());
    let cache = cast.join("cache");
    let assets = cast.join("assets");
    let quarantine = cast.join("quarantine");
    std::fs::set_permissions(&cast, std::fs::Permissions::from_mode(0o750)).unwrap();
    for (directory, existing_mode) in [(&cache, 0o755), (&assets, 0o750), (&quarantine, 0o711)] {
        std::fs::create_dir(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(existing_mode)).unwrap();
    }

    Installation::open(temporary.path(), None).unwrap();
    assert_eq!(mode(&cast), 0o750);
    assert_eq!(mode(&cache), 0o755);
    assert_eq!(mode(&assets), 0o750);
    assert_eq!(mode(&quarantine), 0o711);
}

#[test]
fn cache_root_symlink_is_rejected_without_touching_its_target() {
    let temporary = private_installation_tempdir();
    let cast = prepare_cast_parent(temporary.path());
    let target = temporary.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cache = cast.join("cache");
    symlink(&target, &cache).unwrap();

    assert!(matches!(
        Installation::open(temporary.path(), None),
        Err(Error::PrepareDirectory { path, .. }) if path == cache
    ));
    assert_eq!(mode(&target), 0o755);
    assert!(std::fs::symlink_metadata(cache).unwrap().file_type().is_symlink());
}

#[test]
fn cache_root_wrong_kind_is_rejected_without_replacement() {
    let temporary = private_installation_tempdir();
    let cast = prepare_cast_parent(temporary.path());
    let cache = cast.join("cache");
    std::fs::write(&cache, b"not a directory").unwrap();

    assert!(matches!(
        Installation::open(temporary.path(), None),
        Err(Error::PrepareDirectory { path, .. }) if path == cache
    ));
    assert_eq!(std::fs::read(cache).unwrap(), b"not a directory");
}

#[test]
fn custom_cache_root_uses_the_same_owner_and_mode_policy() {
    let temporary = private_installation_tempdir();
    let custom = temporary.path().join("custom-cache");
    std::fs::create_dir(&custom).unwrap();
    std::fs::set_permissions(&custom, std::fs::Permissions::from_mode(0o775)).unwrap();

    let error = Installation::open(temporary.path(), Some(custom.clone())).unwrap_err();
    assert!(matches!(
        error,
        Error::ValidateCacheDirectory { path, source }
            if path == custom && source.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(mode(&custom), 0o775);
}

#[test]
fn custom_cache_symlink_is_rejected() {
    let temporary = private_installation_tempdir();
    let custom_target = temporary.path().join("custom-target");
    let custom_link = temporary.path().join("custom-link");
    std::fs::create_dir(&custom_target).unwrap();
    std::fs::set_permissions(&custom_target, std::fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&custom_target, &custom_link).unwrap();

    assert!(matches!(
        Installation::open(temporary.path(), Some(custom_link.clone())),
        Err(Error::ValidateCacheDirectory { path, .. }) if path == custom_link
    ));
}

#[test]
fn directory_policy_rejects_a_wrong_owner() {
    let temporary = tempfile::tempdir().unwrap();
    let metadata = std::fs::metadata(temporary.path()).unwrap();
    let wrong_owner = metadata.uid().wrapping_add(1);

    let error = require_controlled_directory_metadata(&metadata, temporary.path(), wrong_owner).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}
