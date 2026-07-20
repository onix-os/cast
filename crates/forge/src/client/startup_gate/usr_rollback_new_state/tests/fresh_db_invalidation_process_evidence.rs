//! Raw filesystem and SQLite evidence for invalidation process death.

use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
};

use crate::{
    State, db,
    state::Id,
    transition_journal::{TransitionJournalStore, TransitionRecord},
};

use super::fresh_db_invalidation_process_boundaries::{
    FreshDbInvalidationProcessBoundary, TemporaryRecordContents,
};

const CAST_NAME: &str = ".cast";
const DATABASE_DIRECTORY: &str = ".cast/db";
const JOURNAL_DIRECTORY: &str = ".cast/journal";
const CANONICAL_NAME: &str = "state-transition";
const LOCK_NAME: &str = "state-transition.lock";
const TEMPORARY_PREFIX: &str = ".state-transition.tmp-";
const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

#[derive(Debug, Eq, PartialEq)]
pub(super) struct FreshDatabaseEvidence {
    previous: State,
    candidate: State,
    in_flight: db::state::InFlightTransition,
    candidate_provenance: db::state::MetadataProvenance,
}

impl FreshDatabaseEvidence {
    pub(super) fn capture(database: &db::state::Database, record: &TransitionRecord) -> Self {
        let previous_id = Id::from(record.previous.id.unwrap());
        let candidate_id = Id::from(record.candidate.id.unwrap());
        assert_ne!(previous_id, candidate_id);
        let previous = database.get(previous_id).unwrap();
        let candidate = database.get(candidate_id).unwrap();
        assert!(
            !candidate.selections.is_empty(),
            "process-death candidate must exercise real selection deletion"
        );
        let states = database.all().unwrap();
        assert_eq!(states, vec![previous.clone(), candidate.clone()]);
        let in_flight = database
            .audit_in_flight_transition()
            .unwrap()
            .expect("selected fresh candidate must remain in flight");
        assert_eq!(in_flight.state_id, candidate_id);
        assert_eq!(in_flight.transition_id, record.transition_id);
        let candidate_provenance = database
            .metadata_provenance(candidate_id)
            .unwrap()
            .expect("selected fresh candidate provenance must be present");
        assert_eq!(database.metadata_provenance(previous_id).unwrap(), None);
        assert_selected_present(database, record);
        Self {
            previous,
            candidate,
            in_flight,
            candidate_provenance,
        }
    }

    pub(super) fn assert_recovered(&self, database: &db::state::Database, record: &TransitionRecord) {
        assert_eq!(database.all().unwrap(), vec![self.previous.clone()]);
        assert_eq!(database.audit_in_flight_transition().unwrap(), None);
        assert_eq!(
            database.metadata_provenance(self.previous.id).unwrap(),
            None
        );
        assert_eq!(
            database.metadata_provenance(self.candidate.id).unwrap(),
            None
        );
        assert_joint_absence(database, record);
    }
}

pub(super) fn assert_selected_present(database: &db::state::Database, record: &TransitionRecord) {
    let candidate = Id::from(record.candidate.id.unwrap());
    let observation = database
        .inspect_exact_fresh_transition(candidate, &record.transition_id)
        .unwrap();
    let db::state::ExactFreshTransitionObservation::Present(preimage) = observation else {
        panic!("process-death recovery expected the selected fresh preimage");
    };
    assert!(
        !preimage.state().selections.is_empty(),
        "process-death preimage must retain a nonempty selection set"
    );
}

pub(super) fn assert_joint_absence(database: &db::state::Database, record: &TransitionRecord) {
    let candidate = Id::from(record.candidate.id.unwrap());
    assert!(matches!(
        database.inspect_exact_fresh_transition(candidate, &record.transition_id),
        Ok(db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
    ));
    assert_eq!(database.audit_in_flight_transition().unwrap(), None);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PublicJournalIdentity {
    cast: (u64, u64),
    journal: (u64, u64),
    lock: (u64, u64),
    canonical: (u64, u64),
}

impl PublicJournalIdentity {
    pub(super) fn capture(root: &Path) -> Self {
        let cast = root.join(CAST_NAME);
        let journal = root.join(JOURNAL_DIRECTORY);
        Self {
            cast: directory_identity(&cast),
            journal: directory_identity(&journal),
            lock: file_identity(&journal.join(LOCK_NAME)),
            canonical: file_identity(&journal.join(CANONICAL_NAME)),
        }
    }

    pub(super) fn assert_same_anchors(self, actual: Self) {
        assert_eq!(actual.cast, self.cast);
        assert_eq!(actual.journal, self.journal);
        assert_eq!(actual.lock, self.lock);
    }

    pub(super) fn assert_crash_identity(
        self,
        actual: Self,
        boundary: FreshDbInvalidationProcessBoundary,
    ) {
        self.assert_same_anchors(actual);
        if boundary.canonical_is_source() {
            assert_eq!(actual.canonical, self.canonical);
        } else {
            assert_ne!(actual.canonical, self.canonical);
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct RawJournalInventory {
    canonical: Vec<u8>,
    temporary: Option<Vec<u8>>,
}

impl RawJournalInventory {
    /// Read names and bytes directly. This deliberately never opens a journal
    /// store, so stale temporary evidence is observed before recovery cleanup.
    pub(super) fn capture(root: &Path) -> Self {
        let journal = root.join(JOURNAL_DIRECTORY);
        let mut names = fs::read_dir(&journal)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        assert!(names.contains(&OsString::from(CANONICAL_NAME)));
        assert!(names.contains(&OsString::from(LOCK_NAME)));
        let temporaries = names
            .iter()
            .filter(|name| valid_temporary_name(name))
            .cloned()
            .collect::<Vec<_>>();
        assert!(temporaries.len() <= 1, "unexpected raw temporary inventory: {names:?}");
        assert_eq!(names.len(), 2 + temporaries.len(), "unexpected raw journal names: {names:?}");
        let temporary = temporaries
            .first()
            .map(|name| fs::read(journal.join(name)).unwrap());
        Self {
            canonical: fs::read(journal.join(CANONICAL_NAME)).unwrap(),
            temporary,
        }
    }

    pub(super) fn assert_after_crash(
        &self,
        boundary: FreshDbInvalidationProcessBoundary,
        source: &[u8],
        successor: &[u8],
    ) {
        let expected_canonical = if boundary.canonical_is_source() {
            source
        } else {
            successor
        };
        assert_eq!(self.canonical, expected_canonical);
        let expected_temporary = match boundary.temporary_contents() {
            Some(TemporaryRecordContents::Source) => Some(source),
            Some(TemporaryRecordContents::Successor) => Some(successor),
            None => None,
        };
        assert_eq!(self.temporary.as_deref(), expected_temporary);
    }

    pub(super) fn assert_clean_successor(&self, successor: &[u8]) {
        assert_eq!(self.canonical, successor);
        assert_eq!(self.temporary, None);
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct RootAbiSnapshot([RootAbiLink; 5]);

impl RootAbiSnapshot {
    pub(super) fn capture(root: &Path) -> Self {
        Self(ROOT_ABI.map(|(name, expected)| {
            let link = root.join(name);
            let metadata = fs::symlink_metadata(&link).unwrap();
            assert!(metadata.file_type().is_symlink());
            let target = fs::read_link(&link).unwrap();
            assert_eq!(target, Path::new(expected));
            RootAbiLink {
                name,
                target,
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                links: metadata.nlink(),
            }
        }))
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RootAbiLink {
    name: &'static str,
    target: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct StableNamespaceSnapshot(Vec<NamespaceEntry>);

impl StableNamespaceSnapshot {
    pub(super) fn capture(root: &Path) -> Self {
        let mut entries = Vec::new();
        snapshot_directory(root, root, &mut entries);
        Self(entries)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct NamespaceEntry {
    relative: PathBuf,
    kind: NamespaceEntryKind,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NamespaceEntryKind {
    Directory,
    File,
    Symlink,
}

fn snapshot_directory(root: &Path, directory: &Path, output: &mut Vec<NamespaceEntry>) {
    let mut children = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    for path in children {
        let relative = path.strip_prefix(root).unwrap().to_owned();
        if relative.starts_with(Path::new(JOURNAL_DIRECTORY)) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).unwrap();
        let file_type = metadata.file_type();
        let (kind, payload) = if file_type.is_dir() {
            (NamespaceEntryKind::Directory, Vec::new())
        } else if file_type.is_file() {
            (NamespaceEntryKind::File, fs::read(&path).unwrap())
        } else if file_type.is_symlink() {
            (
                NamespaceEntryKind::Symlink,
                fs::read_link(&path).unwrap().as_os_str().as_bytes().to_vec(),
            )
        } else {
            panic!("unexpected process-death namespace entry at {}", path.display());
        };
        output.push(NamespaceEntry {
            relative: relative.clone(),
            kind,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: if file_type.is_dir() { 0 } else { metadata.len() },
            payload,
        });
        if file_type.is_dir() && relative != Path::new(DATABASE_DIRECTORY) {
            snapshot_directory(root, &path, output);
        }
    }
}

pub(super) fn assert_journal_reopenable(root: &Path, expected: &TransitionRecord) {
    let installation = crate::Installation::open(root, None).unwrap();
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::try_open_in_retained_cast(cast, root).unwrap();
    assert_eq!(journal.load().unwrap(), Some(expected.clone()));
}

fn valid_temporary_name(name: &OsStr) -> bool {
    let Some(tail) = name.as_bytes().strip_prefix(TEMPORARY_PREFIX.as_bytes()) else {
        return false;
    };
    tail.len() == 8 + 1 + 16
        && tail[8] == b'-'
        && tail[..8]
            .iter()
            .chain(&tail[9..])
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

pub(super) fn canonical_path(root: &Path) -> PathBuf {
    root.join(JOURNAL_DIRECTORY).join(CANONICAL_NAME)
}

fn directory_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir(), "{} is not a directory", path.display());
    (metadata.dev(), metadata.ino())
}

fn file_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file(), "{} is not a regular file", path.display());
    (metadata.dev(), metadata.ino())
}
