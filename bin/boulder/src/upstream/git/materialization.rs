// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Canonical normalization and hashing for a checked-out Git tree.

use std::{
    fs::Permissions,
    io::{self, Read},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt;
use sha2::{Digest, Sha256};
use thiserror::Error;

const DOMAIN: &[u8] = b"os-tools-git-materialization-v1\0";
const DIRECTORY_TAG: u8 = 0;
const REGULAR_TAG: u8 = 1;
const SYMLINK_TAG: u8 = 2;
const DIRECTORY_MODE: u32 = 0o755;
const EXECUTABLE_MODE: u32 = 0o755;
const REGULAR_MODE: u32 = 0o644;
const SYMLINK_MODE: u32 = 0o777;

/// Normalize one exported Git tree and return its canonical SHA-256 digest.
///
/// The initial scan is deliberately separate from mutation: a hard link or
/// special inode anywhere in the tree rejects the complete export before a
/// mode or timestamp is changed. The final scan is the sole source of bytes
/// admitted to the digest.
pub(super) fn normalize_and_hash(root: &Path, source_date_epoch: i64) -> Result<String, Error> {
    let audited = scan_tree(root, false)?;
    normalize_entries(&audited, source_date_epoch)?;

    let normalized = scan_tree(root, true)?;
    require_same_tree(&audited, &normalized)?;
    let digest = hash_tree(&normalized)?;

    // Reading directories, regular files, and symlink targets may update
    // atime. Reapply the frozen timestamp without changing semantic bytes,
    // then prove the result remained the audited tree.
    normalize_entries(&normalized, source_date_epoch)?;
    verify_normalized_tree(&normalized, source_date_epoch)?;

    Ok(hex::encode(digest))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    relative: Vec<u8>,
    identity: Identity,
    kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryKind {
    Directory,
    Regular { executable: bool, length: u64 },
    Symlink { target: Vec<u8> },
}

impl EntryKind {
    fn tag(&self) -> u8 {
        match self {
            Self::Directory => DIRECTORY_TAG,
            Self::Regular { .. } => REGULAR_TAG,
            Self::Symlink { .. } => SYMLINK_TAG,
        }
    }

    fn normalized_mode(&self) -> u32 {
        match self {
            Self::Directory => DIRECTORY_MODE,
            Self::Regular { executable: true, .. } => EXECUTABLE_MODE,
            Self::Regular { executable: false, .. } => REGULAR_MODE,
            Self::Symlink { .. } => SYMLINK_MODE,
        }
    }

    fn is_directory(&self) -> bool {
        matches!(self, Self::Directory)
    }
}

fn scan_tree(root: &Path, require_normalized_modes: bool) -> Result<Vec<Entry>, Error> {
    let root_metadata = inspect(root)?;
    if !root_metadata.file_type().is_dir() {
        return Err(Error::RootNotDirectory(root.to_owned()));
    }

    let mut entries = Vec::new();
    for item in walkdir::WalkDir::new(root).follow_links(false) {
        let item = item.map_err(|source| Error::Walk { source })?;
        let path = item.path().to_owned();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| Error::PathOutsideRoot {
                root: root.to_owned(),
                path: path.clone(),
            })?
            .as_os_str()
            .as_bytes()
            .to_vec();
        let metadata = inspect(&path)?;
        let kind = classify(&path, &metadata)?;
        require_single_link(&path, &metadata, &kind)?;
        if require_normalized_modes {
            require_normalized_mode(&path, &metadata, &kind)?;
        }
        entries.push(Entry {
            path,
            relative,
            identity: Identity::from_metadata(&metadata),
            kind,
        });
    }

    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    for adjacent in entries.windows(2) {
        if adjacent[0].relative == adjacent[1].relative {
            return Err(Error::DuplicatePath(adjacent[0].path.clone()));
        }
    }
    Ok(entries)
}

fn inspect(path: &Path) -> Result<std::fs::Metadata, Error> {
    fs::symlink_metadata(path).map_err(|source| Error::Io {
        operation: "inspect Git materialization entry",
        path: path.to_owned(),
        source,
    })
}

fn classify(path: &Path, metadata: &std::fs::Metadata) -> Result<EntryKind, Error> {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        Ok(EntryKind::Directory)
    } else if file_type.is_file() {
        Ok(EntryKind::Regular {
            executable: metadata.mode() & 0o111 != 0,
            length: metadata.len(),
        })
    } else if file_type.is_symlink() {
        let target = fs::read_link(path).map_err(|source| Error::Io {
            operation: "read Git materialization symlink",
            path: path.to_owned(),
            source,
        })?;
        Ok(EntryKind::Symlink {
            target: target.as_os_str().as_bytes().to_vec(),
        })
    } else {
        Err(Error::UnsupportedFileType {
            path: path.to_owned(),
            kind: special_file_type(&file_type),
        })
    }
}

fn special_file_type(file_type: &std::fs::FileType) -> &'static str {
    if file_type.is_fifo() {
        "FIFO"
    } else if file_type.is_socket() {
        "socket"
    } else if file_type.is_block_device() {
        "block device"
    } else if file_type.is_char_device() {
        "character device"
    } else {
        "unknown special inode"
    }
}

fn require_single_link(path: &Path, metadata: &std::fs::Metadata, kind: &EntryKind) -> Result<(), Error> {
    if !kind.is_directory() && metadata.nlink() != 1 {
        Err(Error::UnexpectedLinkCount {
            path: path.to_owned(),
            links: metadata.nlink(),
        })
    } else {
        Ok(())
    }
}

fn require_normalized_mode(path: &Path, metadata: &std::fs::Metadata, kind: &EntryKind) -> Result<(), Error> {
    if matches!(kind, EntryKind::Symlink { .. }) {
        return Ok(());
    }
    let expected = kind.normalized_mode();
    let actual = metadata.mode() & 0o7777;
    if actual == expected {
        Ok(())
    } else {
        Err(Error::ModeNotNormalized {
            path: path.to_owned(),
            expected,
            actual,
        })
    }
}

fn normalize_entries(entries: &[Entry], source_date_epoch: i64) -> Result<(), Error> {
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);

    // Children precede their directories, so directory timestamps are the
    // final metadata operation for each subtree.
    for entry in entries.iter().rev() {
        match &entry.kind {
            EntryKind::Symlink { .. } => {
                require_path_matches(entry, false)?;
                filetime::set_symlink_file_times(&entry.path, timestamp, timestamp).map_err(|source| Error::Io {
                    operation: "normalize Git materialization symlink timestamp",
                    path: entry.path.clone(),
                    source,
                })?;
                // A post-write read_link would itself change the symlink's
                // freshly normalized atime. Replacing a symlink requires a new
                // inode, so lstat identity and type are the race check here.
                let metadata = inspect(&entry.path)?;
                if Identity::from_metadata(&metadata) != entry.identity || !metadata.file_type().is_symlink() {
                    return Err(Error::EntryChanged(entry.path.clone()));
                }
                require_single_link(&entry.path, &metadata, &entry.kind)?;
            }
            EntryKind::Directory | EntryKind::Regular { .. } => {
                let file = open_nofollow(entry)?;
                require_handle_matches(
                    entry,
                    &file.metadata().map_err(|source| Error::Io {
                        operation: "inspect opened Git materialization entry",
                        path: entry.path.clone(),
                        source,
                    })?,
                    false,
                )?;
                file.set_permissions(Permissions::from_mode(entry.kind.normalized_mode()))
                    .map_err(|source| Error::Io {
                        operation: "normalize Git materialization mode",
                        path: entry.path.clone(),
                        source,
                    })?;
                filetime::set_file_handle_times(file.file(), Some(timestamp), Some(timestamp)).map_err(|source| {
                    Error::Io {
                        operation: "normalize Git materialization timestamp",
                        path: entry.path.clone(),
                        source,
                    }
                })?;
                require_handle_matches(
                    entry,
                    &file.metadata().map_err(|source| Error::Io {
                        operation: "verify opened Git materialization entry",
                        path: entry.path.clone(),
                        source,
                    })?,
                    true,
                )?;
            }
        }
    }
    Ok(())
}

fn open_nofollow(entry: &Entry) -> Result<fs::File, Error> {
    let mut flags = nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK;
    if entry.kind.is_directory() {
        flags |= nix::libc::O_DIRECTORY;
    }
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(&entry.path)
        .map_err(|source| Error::Io {
            operation: "open Git materialization entry without following symlinks",
            path: entry.path.clone(),
            source,
        })
}

fn require_path_matches(entry: &Entry, normalized_mode_required: bool) -> Result<std::fs::Metadata, Error> {
    let metadata = inspect(&entry.path)?;
    let kind = classify(&entry.path, &metadata)?;
    require_single_link(&entry.path, &metadata, &kind)?;
    if normalized_mode_required {
        require_normalized_mode(&entry.path, &metadata, &kind)?;
    }
    if Identity::from_metadata(&metadata) != entry.identity || kind != entry.kind {
        return Err(Error::EntryChanged(entry.path.clone()));
    }
    Ok(metadata)
}

fn require_handle_matches(entry: &Entry, metadata: &std::fs::Metadata, require_normalized: bool) -> Result<(), Error> {
    let kind = if metadata.file_type().is_dir() {
        EntryKind::Directory
    } else if metadata.file_type().is_file() {
        EntryKind::Regular {
            executable: metadata.mode() & 0o111 != 0,
            length: metadata.len(),
        }
    } else {
        return Err(Error::EntryChanged(entry.path.clone()));
    };
    require_single_link(&entry.path, metadata, &kind)?;
    if require_normalized {
        require_normalized_mode(&entry.path, metadata, &kind)?;
    }
    if Identity::from_metadata(metadata) != entry.identity || kind != entry.kind {
        return Err(Error::EntryChanged(entry.path.clone()));
    }
    Ok(())
}

fn require_same_tree(audited: &[Entry], normalized: &[Entry]) -> Result<(), Error> {
    if audited.len() != normalized.len() {
        return Err(Error::TreeChanged);
    }
    for (expected, actual) in audited.iter().zip(normalized) {
        if expected.relative != actual.relative || expected.identity != actual.identity || expected.kind != actual.kind
        {
            return Err(Error::EntryChanged(actual.path.clone()));
        }
    }
    Ok(())
}

fn hash_tree(entries: &[Entry]) -> Result<[u8; 32], Error> {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hash_length(
        &mut hasher,
        entries.len(),
        "entry count",
        &entries.first().expect("a scanned tree includes its root").path,
    )?;

    for entry in entries {
        require_path_matches(entry, true)?;
        hasher.update([entry.kind.tag()]);
        hash_length(&mut hasher, entry.relative.len(), "relative path", &entry.path)?;
        hasher.update(&entry.relative);
        hasher.update(entry.kind.normalized_mode().to_le_bytes());

        match &entry.kind {
            EntryKind::Directory => {}
            EntryKind::Regular { length, .. } => hash_regular(entry, *length, &mut hasher)?,
            EntryKind::Symlink { target } => hash_symlink(entry, target, &mut hasher)?,
        }
    }

    Ok(hasher.finalize().into())
}

fn hash_length(hasher: &mut Sha256, length: usize, field: &'static str, path: &Path) -> Result<(), Error> {
    let length = u64::try_from(length).map_err(|_| Error::LengthNotRepresentable {
        field,
        path: path.to_owned(),
    })?;
    hasher.update(length.to_le_bytes());
    Ok(())
}

fn hash_regular(entry: &Entry, expected_length: u64, hasher: &mut Sha256) -> Result<(), Error> {
    let mut file = open_nofollow(entry)?;
    let before = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git materialization file before hashing",
        path: entry.path.clone(),
        source,
    })?;
    require_handle_matches(entry, &before, true)?;
    if before.len() != expected_length {
        return Err(Error::FileLengthChanged {
            path: entry.path.clone(),
            expected: expected_length,
            actual: before.len(),
        });
    }

    hasher.update(expected_length.to_le_bytes());
    let mut read_length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while read_length < expected_length {
        let remaining = expected_length - read_length;
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).expect("buffer length fits usize");
        let read = match file.read(&mut buffer[..limit]) {
            Ok(0) => {
                return Err(Error::FileLengthChanged {
                    path: entry.path.clone(),
                    expected: expected_length,
                    actual: read_length,
                });
            }
            Ok(read) => read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(Error::Io {
                    operation: "read Git materialization file",
                    path: entry.path.clone(),
                    source,
                });
            }
        };
        hasher.update(&buffer[..read]);
        read_length += u64::try_from(read).expect("buffer read length fits u64");
    }

    let mut extra = [0_u8; 1];
    let extra_read = loop {
        match file.read(&mut extra) {
            Ok(read) => break read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(Error::Io {
                    operation: "verify Git materialization file length",
                    path: entry.path.clone(),
                    source,
                });
            }
        }
    };
    let after = file.metadata().map_err(|source| Error::Io {
        operation: "inspect Git materialization file after hashing",
        path: entry.path.clone(),
        source,
    })?;
    if extra_read != 0 || after.len() != expected_length {
        let observed_extra = if extra_read == 0 { 0 } else { 1 };
        return Err(Error::FileLengthChanged {
            path: entry.path.clone(),
            expected: expected_length,
            actual: after.len().max(expected_length + observed_extra),
        });
    }
    if content_stamp(&before) != content_stamp(&after) {
        return Err(Error::FileChangedDuringHash(entry.path.clone()));
    }
    require_handle_matches(entry, &after, true)?;
    Ok(())
}

fn content_stamp(metadata: &std::fs::Metadata) -> (u64, i64, i64, i64, i64) {
    (
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn hash_symlink(entry: &Entry, expected_target: &[u8], hasher: &mut Sha256) -> Result<(), Error> {
    let target = fs::read_link(&entry.path).map_err(|source| Error::Io {
        operation: "read Git materialization symlink while hashing",
        path: entry.path.clone(),
        source,
    })?;
    let target = target.as_os_str().as_bytes();
    if target != expected_target {
        return Err(Error::EntryChanged(entry.path.clone()));
    }
    hash_length(hasher, target.len(), "symlink target", &entry.path)?;
    hasher.update(target);
    require_path_matches(entry, true)?;
    Ok(())
}

fn verify_normalized_tree(entries: &[Entry], source_date_epoch: i64) -> Result<(), Error> {
    for entry in entries {
        // Reading a symlink target updates the symlink inode's atime on Linux.
        // The final normalization pass already verified the target on both
        // sides of the timestamp write, so this last check deliberately uses
        // only lstat metadata and inode identity.
        let metadata = inspect(&entry.path)?;
        let same_kind = match &entry.kind {
            EntryKind::Directory => metadata.file_type().is_dir(),
            EntryKind::Regular { executable, length } => {
                metadata.file_type().is_file()
                    && metadata.len() == *length
                    && (metadata.mode() & 0o111 != 0) == *executable
            }
            EntryKind::Symlink { .. } => metadata.file_type().is_symlink(),
        };
        if Identity::from_metadata(&metadata) != entry.identity || !same_kind {
            return Err(Error::EntryChanged(entry.path.clone()));
        }
        require_single_link(&entry.path, &metadata, &entry.kind)?;
        require_normalized_mode(&entry.path, &metadata, &entry.kind)?;
        if metadata.atime() != source_date_epoch
            || metadata.atime_nsec() != 0
            || metadata.mtime() != source_date_epoch
            || metadata.mtime_nsec() != 0
        {
            return Err(Error::TimestampNotNormalized {
                path: entry.path.clone(),
                expected: source_date_epoch,
                atime: metadata.atime(),
                atime_nsec: metadata.atime_nsec(),
                mtime: metadata.mtime(),
                mtime_nsec: metadata.mtime_nsec(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("Git materialization root is not a directory: {0:?}")]
    RootNotDirectory(PathBuf),
    #[error("Git materialization entry {path:?} has unsupported type {kind}")]
    UnsupportedFileType { path: PathBuf, kind: &'static str },
    #[error("Git materialization non-directory {path:?} has link count {links}; expected exactly one")]
    UnexpectedLinkCount { path: PathBuf, links: u64 },
    #[error("Git materialization entry {0:?} changed during normalization or hashing")]
    EntryChanged(PathBuf),
    #[error("Git materialization tree changed during normalization or hashing")]
    TreeChanged,
    #[error("Git materialization tree contains duplicate path {0:?}")]
    DuplicatePath(PathBuf),
    #[error("Git materialization path {path:?} is not below root {root:?}")]
    PathOutsideRoot { root: PathBuf, path: PathBuf },
    #[error("Git materialization {field} length for {path:?} cannot be represented as u64")]
    LengthNotRepresentable { field: &'static str, path: PathBuf },
    #[error("Git materialization entry {path:?} has mode {actual:#06o}; expected {expected:#06o}")]
    ModeNotNormalized { path: PathBuf, expected: u32, actual: u32 },
    #[error("Git materialization file {path:?} changed length while hashing (expected {expected}, found {actual})")]
    FileLengthChanged { path: PathBuf, expected: u64, actual: u64 },
    #[error("Git materialization file {0:?} changed while its bytes were being hashed")]
    FileChangedDuringHash(PathBuf),
    #[error(
        "Git materialization entry {path:?} did not retain timestamp {expected}.0 (atime={atime}.{atime_nsec}, mtime={mtime}.{mtime_nsec})"
    )]
    TimestampNotNormalized {
        path: PathBuf,
        expected: i64,
        atime: i64,
        atime_nsec: i64,
        mtime: i64,
        mtime_nsec: i64,
    },
    #[error("walk Git materialization tree: {source}")]
    Walk {
        #[source]
        source: walkdir::Error,
    },
    #[error("{operation} at {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        os::unix::{
            ffi::OsStringExt,
            fs::{MetadataExt, PermissionsExt, symlink},
            net::UnixListener,
        },
    };

    use nix::{sys::stat::Mode, unistd::mkfifo};

    use super::*;

    const EPOCH: i64 = 1_700_000_000;

    #[test]
    fn empty_tree_has_a_stable_golden_digest() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            normalize_and_hash(root.path(), EPOCH).unwrap(),
            "badf0db4c0a8fd2cde62d7893df1313fcdaca41f8b9cab21c2e58f53c033c908"
        );
    }

    #[test]
    fn order_non_utf8_permissions_and_timestamps_normalize_identically() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let raw_name = OsString::from_vec(b"non-utf8-\xff".to_vec());
        create_equivalent_tree(first.path(), &raw_name, false, 111);
        create_equivalent_tree(second.path(), &raw_name, true, 222);

        let first_hash = normalize_and_hash(first.path(), EPOCH).unwrap();
        let second_hash = normalize_and_hash(second.path(), EPOCH).unwrap();
        assert_eq!(first_hash, second_hash);

        for root in [first.path(), second.path()] {
            assert_mode(root, DIRECTORY_MODE);
            assert_mode(&root.join("nested"), DIRECTORY_MODE);
            assert_mode(&root.join(&raw_name), REGULAR_MODE);
            assert_mode(&root.join("executable"), EXECUTABLE_MODE);
            for path in [
                root.to_owned(),
                root.join("nested"),
                root.join(&raw_name),
                root.join("executable"),
                root.join("link"),
            ] {
                assert_timestamp(&path, EPOCH);
            }
        }
    }

    #[test]
    fn content_path_mode_type_and_symlink_target_are_semantic() {
        let baseline = digest_with(|_| {});
        let mutations = [
            digest_with(|root| fs::write(root.join("regular"), b"bravo").unwrap()),
            digest_with(|root| fs::write(root.join("regular"), b"longer").unwrap()),
            digest_with(|root| fs::rename(root.join("regular"), root.join("renamed")).unwrap()),
            digest_with(|root| fs::set_permissions(root.join("regular"), Permissions::from_mode(0o755)).unwrap()),
            digest_with(|root| {
                fs::remove_file(root.join("link")).unwrap();
                symlink("executable", root.join("link")).unwrap();
            }),
            digest_with(|root| {
                fs::remove_file(root.join("kind")).unwrap();
                fs::create_dir(root.join("kind")).unwrap();
            }),
        ];
        for mutation in mutations {
            assert_ne!(mutation, baseline);
        }
    }

    #[test]
    fn hard_links_reject_the_whole_tree_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("a-original");
        fs::write(&original, b"shared").unwrap();
        fs::set_permissions(&original, Permissions::from_mode(0o600)).unwrap();
        let old = filetime::FileTime::from_unix_time(123, 0);
        filetime::set_file_times(&original, old, old).unwrap();
        fs::hard_link(&original, root.path().join("b-link")).unwrap();

        assert!(matches!(
            normalize_and_hash(root.path(), EPOCH),
            Err(Error::UnexpectedLinkCount { links: 2, .. })
        ));
        assert_mode(&original, 0o600);
        assert_eq!(fs::metadata(&original).unwrap().mtime(), 123);
    }

    #[test]
    fn fifos_and_sockets_are_rejected_before_mutation() {
        let fifo_root = tempfile::tempdir().unwrap();
        let fifo_sentinel = fifo_root.path().join("a-sentinel");
        fs::write(&fifo_sentinel, b"sentinel").unwrap();
        fs::set_permissions(&fifo_sentinel, Permissions::from_mode(0o600)).unwrap();
        mkfifo(&fifo_root.path().join("z-fifo"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        assert!(matches!(
            normalize_and_hash(fifo_root.path(), EPOCH),
            Err(Error::UnsupportedFileType { kind: "FIFO", .. })
        ));
        assert_mode(&fifo_sentinel, 0o600);

        let socket_root = tempfile::tempdir().unwrap();
        let socket_sentinel = socket_root.path().join("a-sentinel");
        fs::write(&socket_sentinel, b"sentinel").unwrap();
        fs::set_permissions(&socket_sentinel, Permissions::from_mode(0o600)).unwrap();
        let _listener = UnixListener::bind(socket_root.path().join("z-socket")).unwrap();
        assert!(matches!(
            normalize_and_hash(socket_root.path(), EPOCH),
            Err(Error::UnsupportedFileType { kind: "socket", .. })
        ));
        assert_mode(&socket_sentinel, 0o600);
    }

    #[test]
    fn symlinks_are_hashed_and_timestamped_without_following_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("tree");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::write(&outside, b"outside").unwrap();
        fs::set_permissions(&outside, Permissions::from_mode(0o600)).unwrap();
        let old = filetime::FileTime::from_unix_time(123, 0);
        filetime::set_file_times(&outside, old, old).unwrap();
        symlink("../outside", root.join("link")).unwrap();

        normalize_and_hash(&root, EPOCH).unwrap();

        assert!(
            fs::symlink_metadata(root.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_timestamp(&root.join("link"), EPOCH);
        assert_mode(&outside, 0o600);
        assert_eq!(fs::metadata(&outside).unwrap().mtime(), 123);
    }

    fn create_equivalent_tree(root: &Path, raw_name: &OsString, reverse: bool, timestamp: i64) {
        fs::create_dir(root.join("nested")).unwrap();
        let files = if reverse {
            vec![
                (PathBuf::from("executable"), b"execute".as_slice()),
                (PathBuf::from(raw_name), b"raw".as_slice()),
            ]
        } else {
            vec![
                (PathBuf::from(raw_name), b"raw".as_slice()),
                (PathBuf::from("executable"), b"execute".as_slice()),
            ]
        };
        for (path, bytes) in files {
            fs::write(root.join(path), bytes).unwrap();
        }
        symlink(raw_name, root.join("link")).unwrap();

        fs::set_permissions(root, Permissions::from_mode(if reverse { 0o777 } else { 0o700 })).unwrap();
        fs::set_permissions(
            root.join("nested"),
            Permissions::from_mode(if reverse { 0o775 } else { 0o700 }),
        )
        .unwrap();
        fs::set_permissions(
            root.join(raw_name),
            Permissions::from_mode(if reverse { 0o664 } else { 0o600 }),
        )
        .unwrap();
        fs::set_permissions(
            root.join("executable"),
            Permissions::from_mode(if reverse { 0o777 } else { 0o711 }),
        )
        .unwrap();

        let old = filetime::FileTime::from_unix_time(timestamp, 0);
        for path in [
            root.to_owned(),
            root.join("nested"),
            root.join(raw_name),
            root.join("executable"),
        ] {
            filetime::set_file_times(path, old, old).unwrap();
        }
        filetime::set_symlink_file_times(root.join("link"), old, old).unwrap();
    }

    fn digest_with(mutate: impl FnOnce(&Path)) -> String {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("regular"), b"alpha").unwrap();
        fs::write(root.path().join("executable"), b"execute").unwrap();
        fs::set_permissions(root.path().join("executable"), Permissions::from_mode(0o755)).unwrap();
        fs::write(root.path().join("kind"), b"kind").unwrap();
        symlink("regular", root.path().join("link")).unwrap();
        mutate(root.path());
        normalize_and_hash(root.path(), EPOCH).unwrap()
    }

    fn assert_mode(path: &Path, expected: u32) {
        assert_eq!(fs::symlink_metadata(path).unwrap().mode() & 0o7777, expected);
    }

    fn assert_timestamp(path: &Path, expected: i64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert_eq!(metadata.atime(), expected);
        assert_eq!(metadata.atime_nsec(), 0);
        assert_eq!(metadata.mtime(), expected);
        assert_eq!(metadata.mtime_nsec(), 0);
    }
}
