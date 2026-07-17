use super::*;

fn retain_directory(path: &Path) -> std::fs::File {
    let encoded = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    openat2_file(
        nix::libc::AT_FDCWD,
        &encoded,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )
    .unwrap()
}

#[test]
fn once_chmod_mutates_the_retained_inode_not_its_public_name_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let public = temporary.path().join("candidate");
    let displaced = temporary.path().join("displaced");
    std::fs::create_dir(&public).unwrap();
    std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o500)).unwrap();
    let retained = retain_directory(&public);
    let retained_before = retained.metadata().unwrap();

    std::fs::rename(&public, &displaced).unwrap();
    std::fs::create_dir(&public).unwrap();
    std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o711)).unwrap();
    let replacement_before = std::fs::symlink_metadata(&public).unwrap();

    chmod_path_descriptor_once(&retained, 0o700).unwrap();

    let retained_after = retained.metadata().unwrap();
    let displaced_after = std::fs::symlink_metadata(&displaced).unwrap();
    let replacement_after = std::fs::symlink_metadata(&public).unwrap();
    assert_eq!(
        (retained_after.dev(), retained_after.ino()),
        (retained_before.dev(), retained_before.ino())
    );
    assert_eq!(
        (displaced_after.dev(), displaced_after.ino()),
        (retained_before.dev(), retained_before.ino())
    );
    assert_eq!(retained_after.permissions().mode() & 0o7777, 0o700);
    assert_eq!(
        (replacement_after.dev(), replacement_after.ino()),
        (replacement_before.dev(), replacement_before.ino())
    );
    assert_eq!(replacement_after.permissions().mode() & 0o7777, 0o711);
}

#[test]
fn once_chmod_rejects_an_out_of_range_mode_before_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let retained = retain_directory(temporary.path());
    let before = retained.metadata().unwrap();

    let error = chmod_path_descriptor_once(&retained, 0o10000).unwrap_err();

    let after = retained.metadata().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(after.permissions().mode() & 0o7777, 0o500);
}
