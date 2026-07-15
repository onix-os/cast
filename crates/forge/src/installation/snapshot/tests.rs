use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    client::{Client, Error as ClientError},
    test_support::private_installation_tempdir,
};

use super::super::{Error, Installation, LOCKFILE_MODE, PRIVATE_DIRECTORY_MODE, lockfile};

fn provision(root: &Path, custom_cache: Option<&Path>) {
    let installation = Installation::open(root, custom_cache.map(Path::to_path_buf)).unwrap();
    drop(installation);
}

fn private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
}

fn exclusive_probe_is_blocked(path: &Path) -> bool {
    let file = fs::File::open(path).unwrap();
    lockfile::try_acquire_exclusive_file(file).unwrap().is_none()
}

#[test]
fn two_readers_share_global_and_custom_cache_locks_until_the_last_reader_drops() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().join("root");
    let custom_cache = temporary.path().join("custom-cache");
    private_directory(&root);
    private_directory(&custom_cache);
    provision(&root, Some(&custom_cache));

    let first = Installation::open_read_only(&root, Some(custom_cache.clone())).unwrap();
    let second = Installation::open_read_only(&root, Some(custom_cache.clone())).unwrap();
    let global_lock = root.join(".cast/.cast-lockfile");
    let custom_lock = custom_cache.join(".cast-lockfile");

    assert!(exclusive_probe_is_blocked(&global_lock));
    assert!(exclusive_probe_is_blocked(&custom_lock));
    drop(first);
    assert!(exclusive_probe_is_blocked(&global_lock));
    assert!(exclusive_probe_is_blocked(&custom_lock));
    drop(second);
    assert!(!exclusive_probe_is_blocked(&global_lock));
    assert!(!exclusive_probe_is_blocked(&custom_lock));
}

#[test]
fn writable_root_opened_explicitly_read_only_never_becomes_mutable_or_frozen() {
    let temporary = private_installation_tempdir();
    provision(temporary.path(), None);

    let installation = Installation::open_read_only(temporary.path(), None).unwrap();

    assert!(fs::metadata(temporary.path()).unwrap().permissions().mode() & 0o200 != 0);
    assert!(installation.read_only());
    assert!(!installation.is_mutable_system());
    assert!(installation.is_read_only_snapshot());
    assert!(!installation.is_frozen_cache());
    installation.revalidate_read_only_snapshot().unwrap();
}

#[test]
fn mutable_and_frozen_modes_do_not_expose_read_only_snapshot_authority() {
    let temporary = private_installation_tempdir();

    let mutable = Installation::open(temporary.path(), None).unwrap();
    assert!(mutable.is_mutable_system());
    assert!(!mutable.is_read_only_snapshot());
    assert!(matches!(
        mutable.revalidate_read_only_snapshot(),
        Err(Error::ReadOnlySnapshotAuthorityRequired)
    ));
    drop(mutable);

    let frozen = Installation::open_frozen(temporary.path(), None).unwrap();
    assert!(!frozen.is_mutable_system());
    assert!(frozen.is_frozen_cache());
    assert!(!frozen.is_read_only_snapshot());
    assert!(matches!(
        frozen.revalidate_read_only_snapshot(),
        Err(Error::ReadOnlySnapshotAuthorityRequired)
    ));
    drop(frozen);

    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let naturally_read_only = Installation::open(temporary.path(), None).unwrap();
    assert!(naturally_read_only.read_only());
    assert!(!naturally_read_only.is_mutable_system());
    assert!(!naturally_read_only.is_read_only_snapshot());
    assert!(!naturally_read_only.is_frozen_cache());
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
}

#[test]
fn explicit_snapshot_is_rejected_before_client_coordinator_or_database_mutation() {
    let temporary = private_installation_tempdir();
    provision(temporary.path(), None);
    let before = inventory(temporary.path());
    let installation = Installation::open_read_only(temporary.path(), None).unwrap();

    let result = Client::builder("read-only-snapshot", installation).build();

    assert!(matches!(result, Err(ClientError::SystemInstallationRequired)));
    assert_eq!(inventory(temporary.path()), before);
}

#[test]
fn naturally_read_only_open_is_rejected_before_client_coordinator_or_database_mutation() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().join("root");
    private_directory(&root);
    provision(&root, None);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    let before = inventory(&root);
    let installation = Installation::open(&root, None).unwrap();

    let result = Client::builder("naturally-read-only", installation).build();

    assert!(matches!(result, Err(ClientError::SystemInstallationRequired)));
    assert_eq!(inventory(&root), before);
    fs::set_permissions(&root, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
}

#[test]
fn contended_shared_snapshot_lock_has_a_typed_zero_budget_timeout_without_mutation() {
    let temporary = private_installation_tempdir();
    provision(temporary.path(), None);
    let lock_path = temporary.path().join(".cast/.cast-lockfile");
    let holder = lockfile::try_acquire_exclusive_file(fs::File::open(&lock_path).unwrap())
        .unwrap()
        .expect("the provisioner released its exclusive lock");
    let before = inventory(temporary.path());

    let error =
        Installation::open_read_only_with_lock_timeout(temporary.path().to_owned(), None, Duration::ZERO).unwrap_err();

    assert!(matches!(
        error,
        Error::ReadOnlySnapshotLockTimeout { path, timeout }
            if path == lock_path && timeout == Duration::ZERO
    ));
    assert_eq!(inventory(temporary.path()), before);
    drop(holder);
    Installation::open_read_only_with_lock_timeout(temporary.path().to_owned(), None, Duration::ZERO).unwrap();
}

#[test]
fn missing_cast_fails_without_creating_or_changing_any_entry() {
    let temporary = private_installation_tempdir();
    fs::write(temporary.path().join("sentinel"), b"unchanged").unwrap();
    let before = inventory(temporary.path());

    let error = Installation::open_read_only(temporary.path(), None).unwrap_err();

    assert!(matches!(
        error,
        Error::OpenReadOnlySnapshotDirectory { path, source }
            if path == temporary.path().join(".cast") && source.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(!temporary.path().join(".cast").exists());
    assert_eq!(inventory(temporary.path()), before);
}

#[test]
fn missing_default_cache_fails_without_recreating_or_changing_any_entry() {
    let temporary = private_installation_tempdir();
    provision(temporary.path(), None);
    let cache = temporary.path().join(".cast/cache");
    fs::remove_dir_all(&cache).unwrap();
    let before = inventory(temporary.path());

    let error = Installation::open_read_only(temporary.path(), None).unwrap_err();

    assert!(matches!(
        error,
        Error::OpenReadOnlySnapshotDirectory { path, source }
            if path == cache && source.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(!cache.exists());
    assert_eq!(inventory(temporary.path()), before);
}

#[test]
fn missing_global_lock_fails_without_recreating_it() {
    let temporary = private_installation_tempdir();
    provision(temporary.path(), None);
    let lock = temporary.path().join(".cast/.cast-lockfile");
    fs::remove_file(&lock).unwrap();
    let before = inventory(temporary.path());

    let error = Installation::open_read_only(temporary.path(), None).unwrap_err();

    assert!(matches!(
        error,
        Error::OpenReadOnlySnapshotLockfile { path, source }
            if path == lock && source.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(!lock.exists());
    assert_eq!(inventory(temporary.path()), before);
}

#[test]
fn missing_custom_cache_lock_fails_without_recreating_it() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().join("root");
    let custom_cache = temporary.path().join("custom-cache");
    private_directory(&root);
    private_directory(&custom_cache);
    provision(&root, Some(&custom_cache));
    let lock = custom_cache.join(".cast-lockfile");
    fs::remove_file(&lock).unwrap();
    let before = inventory(temporary.path());

    let error = Installation::open_read_only(&root, Some(custom_cache.clone())).unwrap_err();

    assert!(matches!(
        error,
        Error::OpenReadOnlySnapshotLockfile { path, source }
            if path == lock && source.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(!lock.exists());
    assert_eq!(inventory(temporary.path()), before);
}

#[test]
fn missing_custom_cache_directory_fails_without_creating_or_changing_any_entry() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().join("root");
    let missing_cache = temporary.path().join("missing-cache");
    private_directory(&root);
    provision(&root, None);
    let before = inventory(temporary.path());

    let error = Installation::open_read_only(&root, Some(missing_cache.clone())).unwrap_err();

    assert!(matches!(
        error,
        Error::OpenReadOnlySnapshotDirectory { path, source }
            if path == missing_cache && source.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(!missing_cache.exists());
    assert_eq!(inventory(temporary.path()), before);
}

#[test]
fn retained_snapshot_rejects_installation_root_substitution() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().join("root");
    let detached = temporary.path().join("detached-root");
    private_directory(&root);
    provision(&root, None);
    let installation = Installation::open_read_only(&root, None).unwrap();

    fs::rename(&root, &detached).unwrap();
    private_directory(&root);

    assert!(matches!(
        installation.revalidate_read_only_snapshot(),
        Err(Error::ValidateRootDirectory { path, .. }) if path == root
    ));
    assert_ne!(
        fs::metadata(&root).unwrap().ino(),
        fs::metadata(&detached).unwrap().ino()
    );
}

#[test]
fn retained_snapshot_rejects_cast_directory_substitution() {
    let temporary = private_installation_tempdir();
    provision(temporary.path(), None);
    let installation = Installation::open_read_only(temporary.path(), None).unwrap();
    let cast = temporary.path().join(".cast");
    let detached = temporary.path().join("detached-cast");

    fs::rename(&cast, &detached).unwrap();
    private_directory(&cast);

    assert!(matches!(
        installation.revalidate_read_only_snapshot(),
        Err(Error::OpenReadOnlySnapshotDirectory { path, .. }) if path == cast
    ));
    assert_ne!(
        fs::metadata(&cast).unwrap().ino(),
        fs::metadata(&detached).unwrap().ino()
    );
}

#[test]
fn retained_snapshot_rejects_lockfile_substitution() {
    let temporary = private_installation_tempdir();
    provision(temporary.path(), None);
    let installation = Installation::open_read_only(temporary.path(), None).unwrap();
    let lock = temporary.path().join(".cast/.cast-lockfile");
    let detached = temporary.path().join(".cast/detached-lock");

    fs::rename(&lock, &detached).unwrap();
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(LOCKFILE_MODE)).unwrap();

    assert!(matches!(
        installation.revalidate_read_only_snapshot(),
        Err(Error::OpenReadOnlySnapshotLockfile { path, .. }) if path == lock
    ));
    assert_ne!(
        fs::metadata(&lock).unwrap().ino(),
        fs::metadata(&detached).unwrap().ino()
    );
}

#[test]
fn retained_snapshot_rejects_default_cache_directory_substitution() {
    let temporary = private_installation_tempdir();
    provision(temporary.path(), None);
    let installation = Installation::open_read_only(temporary.path(), None).unwrap();
    let cache = temporary.path().join(".cast/cache");
    let detached = temporary.path().join(".cast/detached-cache");

    fs::rename(&cache, &detached).unwrap();
    private_directory(&cache);

    assert!(matches!(
        installation.revalidate_read_only_snapshot(),
        Err(Error::OpenReadOnlySnapshotDirectory { path, .. }) if path == cache
    ));
    assert_ne!(
        fs::metadata(&cache).unwrap().ino(),
        fs::metadata(&detached).unwrap().ino()
    );
}

#[test]
fn retained_snapshot_rejects_custom_cache_directory_substitution() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().join("root");
    let custom_cache = temporary.path().join("custom-cache");
    let detached = temporary.path().join("detached-cache");
    private_directory(&root);
    private_directory(&custom_cache);
    provision(&root, Some(&custom_cache));
    let installation = Installation::open_read_only(&root, Some(custom_cache.clone())).unwrap();

    fs::rename(&custom_cache, &detached).unwrap();
    private_directory(&custom_cache);

    assert!(matches!(
        installation.revalidate_read_only_snapshot(),
        Err(Error::OpenReadOnlySnapshotDirectory { path, .. }) if path == custom_cache
    ));
    assert_ne!(
        fs::metadata(&custom_cache).unwrap().ino(),
        fs::metadata(&detached).unwrap().ino()
    );
}

#[test]
fn retained_snapshot_rejects_custom_cache_lockfile_substitution() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().join("root");
    let custom_cache = temporary.path().join("custom-cache");
    private_directory(&root);
    private_directory(&custom_cache);
    provision(&root, Some(&custom_cache));
    let installation = Installation::open_read_only(&root, Some(custom_cache.clone())).unwrap();
    let lock = custom_cache.join(".cast-lockfile");
    let detached = custom_cache.join("detached-lock");

    fs::rename(&lock, &detached).unwrap();
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(LOCKFILE_MODE)).unwrap();

    assert!(matches!(
        installation.revalidate_read_only_snapshot(),
        Err(Error::OpenReadOnlySnapshotLockfile { path, .. }) if path == lock
    ));
    assert_ne!(
        fs::metadata(&lock).unwrap().ino(),
        fs::metadata(&detached).unwrap().ino()
    );
}

#[test]
fn open_revalidate_clone_and_drop_leave_recursive_metadata_and_contents_unchanged() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().join("root");
    let custom_cache = temporary.path().join("custom-cache");
    private_directory(&root);
    private_directory(&custom_cache);
    provision(&root, Some(&custom_cache));
    fs::write(root.join(".cast/repo/catalog.glu"), b"let catalog = []\n").unwrap();
    fs::write(custom_cache.join("package.stone"), b"stone-bytes").unwrap();
    let before = inventory(temporary.path());

    let installation = Installation::open_read_only(&root, Some(custom_cache.clone())).unwrap();
    installation.revalidate_read_only_snapshot().unwrap();
    let clone = installation.clone();
    drop(installation);
    clone.revalidate_read_only_snapshot().unwrap();
    drop(clone);

    assert_eq!(inventory(temporary.path()), before);
}

#[derive(Debug, Eq, PartialEq)]
struct InventoryEntry {
    path: PathBuf,
    kind: &'static str,
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    symlink_target: Option<PathBuf>,
    contents: Option<Vec<u8>>,
}

fn inventory(root: &Path) -> Vec<InventoryEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut Vec<InventoryEntry>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let file_type = metadata.file_type();
        let kind = if file_type.is_dir() {
            "directory"
        } else if file_type.is_file() {
            "file"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "special"
        };
        let symlink_target = file_type.is_symlink().then(|| fs::read_link(path).unwrap());
        let contents = file_type.is_file().then(|| fs::read(path).unwrap());
        entries.push(InventoryEntry {
            path: path.strip_prefix(root).unwrap().to_owned(),
            kind,
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            symlink_target,
            contents,
        });

        if file_type.is_dir() {
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, entries);
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}
