use std::{
    fs,
    io::Read as _,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{Installation, test_support::private_installation_tempdir};

use super::{CleanReadOnlyJournal, ReadOnlyJournalError};
use crate::transition_journal::TransitionJournalStore;

fn provision(root: &Path, journal: bool) {
    let installation = Installation::open(root, None).unwrap();
    if journal {
        drop(TransitionJournalStore::open_retained(installation.root_directory(), root).unwrap());
    }
    drop(installation);
}

fn names(path: &Path) -> Vec<Vec<u8>> {
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOATIME | nix::libc::O_CLOEXEC)
        .open(path)
        .unwrap();
    let mut names = crate::transition_journal::directory_entries(&directory)
        .unwrap()
        .into_iter()
        .map(|name| name.to_bytes().to_vec())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[derive(Debug, Eq, PartialEq)]
struct NodeSnapshot {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    length: u64,
    accessed_seconds: i64,
    accessed_nanoseconds: i64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    contents: Option<Vec<u8>>,
    entries: Option<Vec<Vec<u8>>>,
}

fn node_snapshot(path: &Path) -> NodeSnapshot {
    let metadata = fs::symlink_metadata(path).unwrap();
    let contents = metadata.file_type().is_file().then(|| read_noatime(path));
    let entries = metadata.file_type().is_dir().then(|| names(path));
    let metadata = fs::symlink_metadata(path).unwrap();
    NodeSnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        group: metadata.gid(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        length: metadata.len(),
        accessed_seconds: metadata.atime(),
        accessed_nanoseconds: metadata.atime_nsec(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        contents,
        entries,
    }
}

fn read_noatime(path: &Path) -> Vec<u8> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOATIME | nix::libc::O_CLOEXEC)
        .open(path)
        .unwrap();
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).unwrap();
    contents
}

fn age_atime(path: &Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    let modified = filetime::FileTime::from_last_modification_time(&metadata);
    filetime::set_file_times(path, filetime::FileTime::from_unix_time(1, 0), modified).unwrap();
}

#[test]
fn absent_and_preexisting_clean_journals_are_retained_without_provisioning() {
    let absent = private_installation_tempdir();
    provision(absent.path(), false);
    let journal_path = absent.path().join(".cast/journal");
    let installation = Installation::open_read_only(absent.path(), None).unwrap();
    let proof = CleanReadOnlyJournal::inspect(&installation).unwrap();
    proof.revalidate(&installation).unwrap();
    drop(proof);
    drop(installation);
    assert!(!journal_path.exists());

    let present = private_installation_tempdir();
    provision(present.path(), true);
    let journal_path = present.path().join(".cast/journal");
    let lock_path = journal_path.join("state-transition.lock");
    age_atime(&journal_path);
    age_atime(&lock_path);
    let before = node_snapshot(&journal_path);
    let before_lock = node_snapshot(&lock_path);
    let installation = Installation::open_read_only(present.path(), None).unwrap();
    let proof = CleanReadOnlyJournal::inspect(&installation).unwrap();
    proof.revalidate(&installation).unwrap();
    drop(proof);
    drop(installation);
    assert_eq!(node_snapshot(&journal_path), before);
    assert_eq!(node_snapshot(&lock_path), before_lock);
}

#[test]
fn valid_canonical_transition_fails_closed_and_is_preserved() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let store = TransitionJournalStore::open_retained(installation.root_directory(), temporary.path()).unwrap();
    let record = crate::transition_journal::tests::creation_record();
    store.create(&record).unwrap();
    drop(store);
    drop(installation);
    let canonical = temporary.path().join(".cast/journal/state-transition");
    let journal = temporary.path().join(".cast/journal");
    let lock = journal.join("state-transition.lock");
    age_atime(&journal);
    age_atime(&lock);
    age_atime(&canonical);
    let before_journal = node_snapshot(&journal);
    let before_lock = node_snapshot(&lock);
    let before = node_snapshot(&canonical);

    let installation = Installation::open_read_only(temporary.path(), None).unwrap();
    assert!(matches!(
        CleanReadOnlyJournal::inspect(&installation),
        Err(ReadOnlyJournalError::UnresolvedTransition { transition })
            if transition == record.transition_id
    ));
    assert_eq!(node_snapshot(&journal), before_journal);
    assert_eq!(node_snapshot(&lock), before_lock);
    assert_eq!(node_snapshot(&canonical), before);
}

#[test]
fn corrupt_canonical_and_interrupted_temporary_fail_closed_unchanged() {
    let corrupt = private_installation_tempdir();
    provision(corrupt.path(), true);
    let canonical = corrupt.path().join(".cast/journal/state-transition");
    fs::write(&canonical, b"corrupt journal").unwrap();
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
    age_atime(&canonical);
    let before = node_snapshot(&canonical);
    let installation = Installation::open_read_only(corrupt.path(), None).unwrap();
    assert!(matches!(
        CleanReadOnlyJournal::inspect(&installation),
        Err(ReadOnlyJournalError::Decode(_))
    ));
    assert_eq!(node_snapshot(&canonical), before);

    let interrupted = private_installation_tempdir();
    provision(interrupted.path(), true);
    let temporary = interrupted
        .path()
        .join(".cast/journal/.state-transition.tmp-00000001-0000000000000001");
    fs::write(&temporary, b"partial").unwrap();
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).unwrap();
    age_atime(&temporary);
    let before = node_snapshot(&temporary);
    let installation = Installation::open_read_only(interrupted.path(), None).unwrap();
    assert!(matches!(
        CleanReadOnlyJournal::inspect(&installation),
        Err(ReadOnlyJournalError::InterruptedTemporary { .. })
    ));
    assert_eq!(node_snapshot(&temporary), before);

    let foreign = private_installation_tempdir();
    provision(foreign.path(), true);
    let evidence = foreign.path().join(".cast/journal/foreign-evidence");
    fs::write(&evidence, b"foreign").unwrap();
    age_atime(&evidence);
    let before = node_snapshot(&evidence);
    let installation = Installation::open_read_only(foreign.path(), None).unwrap();
    assert!(matches!(
        CleanReadOnlyJournal::inspect(&installation),
        Err(ReadOnlyJournalError::UnexpectedEntry(name)) if name == "foreign-evidence"
    ));
    assert_eq!(node_snapshot(&evidence), before);
}
