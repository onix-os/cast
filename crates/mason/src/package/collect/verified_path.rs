use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{self, Read},
    os::unix::fs::{FileTypeExt as _, MetadataExt as _},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use astr::AStr;
use nix::libc;
use stone::{StoneDigestWriterHasher, StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::{
    CollectionLimits, Error,
    filesystem::{
        CollectionContext, Deadline, changed, is_supported_special, metadata, open_entry, open_entry_handle,
        read_symlink_handle, require_snapshot,
    },
    inventory::{DirectoryId, WitnessGraph},
    mutation,
    traversal::{DirectoryHandle, FileSnapshot, NodeIdentity, RootAnchor},
};

#[derive(Debug, Clone)]
pub(super) enum VerifiedKind {
    Regular { hash: u128 },
    Symlink { target: String },
    Directory,
    Special,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedPath {
    pub(super) anchor: Arc<RootAnchor>,
    pub(super) witness: Arc<WitnessGraph>,
    pub(super) parent_id: DirectoryId,
    pub(super) name: OsString,
    pub(super) snapshot: FileSnapshot,
    pub(super) kind: VerifiedKind,
    pub(super) limits: CollectionLimits,
    pub(super) deadline: Arc<Deadline>,
}

impl VerifiedPath {
    pub(super) fn new(
        parent: &DirectoryHandle,
        name: OsString,
        snapshot: FileSnapshot,
        kind: VerifiedKind,
        limits: CollectionLimits,
        deadline: Arc<Deadline>,
    ) -> Self {
        Self {
            anchor: Arc::clone(&parent.anchor),
            witness: Arc::clone(&parent.witness),
            parent_id: parent.witness_id,
            name,
            snapshot,
            kind,
            limits,
            deadline,
        }
    }

    pub(super) fn display_path(&self) -> Result<PathBuf, Error> {
        self.witness.entry_path(self.parent_id, &self.name)
    }

    pub(super) fn open_parent(&self, operation: &'static str) -> Result<File, Error> {
        let path = self.display_path()?;
        self.deadline.check(&path)?;
        self.anchor.verify_path_node()?;
        let parent = self.witness.open_directory(self.parent_id, &path)?;
        let parent_metadata = metadata(&parent, operation, &path)?;
        if NodeIdentity::from_metadata(&parent_metadata) != self.witness.directory_identity(self.parent_id)? {
            return Err(changed(&path, "package entry parent was replaced"));
        }
        self.deadline.check(&path)?;
        Ok(parent)
    }

    pub(super) fn verify(&self) -> Result<(), Error> {
        let path = self.display_path()?;
        let parent = self.open_parent("verify package entry parent")?;
        let handle = open_entry_handle(&parent, &self.name, &path)?;
        let current = metadata(&handle, "verify collected package entry", &path)?;
        if matches!(self.kind, VerifiedKind::Directory) {
            if !current.file_type().is_dir() {
                return Err(changed(&path, "collected directory changed type"));
            }
            return self.witness.require_rewitnessed_directory(
                self.parent_id,
                &self.name,
                self.snapshot,
                FileSnapshot::from_metadata(&current),
                &path,
            );
        }
        require_snapshot(&path, self.snapshot, &current)?;
        match &self.kind {
            VerifiedKind::Regular { .. } if !current.file_type().is_file() => {
                Err(changed(&path, "collected regular file changed type"))
            }
            VerifiedKind::Symlink { target } => {
                if !current.file_type().is_symlink() {
                    return Err(changed(&path, "collected symlink changed type"));
                }
                let context = CollectionContext::detached(self.limits, Arc::clone(&self.deadline));
                let current_target = read_symlink_handle(&handle, &path, &context)?;
                if &current_target == target {
                    Ok(())
                } else {
                    Err(changed(&path, "collected symlink target changed"))
                }
            }
            VerifiedKind::Special if !is_supported_special(&current.file_type()) => {
                Err(changed(&path, "collected special entry changed type"))
            }
            _ => Ok(()),
        }
    }

    pub(super) fn open_regular(&self) -> Result<VerifiedFileReader, Error> {
        let path = self.display_path()?;
        let expected_hash = match self.kind {
            VerifiedKind::Regular { hash } => hash,
            _ => return Err(Error::UnverifiedContent { path }),
        };
        self.verify()?;
        let parent = self.open_parent("open verified package parent")?;
        let parent_metadata = metadata(&parent, "open verified package parent", &path)?;
        let parent_identity = self.witness.directory_identity(self.parent_id)?;
        if NodeIdentity::from_metadata(&parent_metadata) != parent_identity {
            return Err(changed(&path, "package file parent was replaced before emission"));
        }
        let file = open_entry(
            &parent,
            &self.name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
            &path,
        )?;
        let opened = metadata(&file, "stat verified package content", &path)?;
        if !opened.file_type().is_file() {
            return Err(changed(&path, "package content stopped being a regular file"));
        }
        require_snapshot(&path, self.snapshot, &opened)?;
        Ok(VerifiedFileReader {
            file,
            parent,
            anchor: Arc::clone(&self.anchor),
            witness: Arc::clone(&self.witness),
            parent_id: self.parent_id,
            parent_identity,
            name: self.name.clone(),
            path,
            expected: self.snapshot,
            expected_hash,
            hasher: StoneDigestWriterHasher::new(),
            bytes: 0,
            deadline: Arc::clone(&self.deadline),
            exceeded: false,
        })
    }
}

#[derive(Debug)]
pub struct PathInfo {
    pub path: PathBuf,
    pub target_path: PathBuf,
    pub layout: StonePayloadLayoutRecord,
    pub size: u64,
    pub package: Arc<str>,
    pub(crate) verified: Option<VerifiedPath>,
}

impl PathInfo {
    pub(super) fn verified(
        path: PathBuf,
        relative: PathBuf,
        layout: StonePayloadLayoutRecord,
        size: u64,
        package: Arc<str>,
        verified: VerifiedPath,
    ) -> Self {
        Self {
            path,
            target_path: Path::new("/").join(relative),
            layout,
            size,
            package,
            verified: Some(verified),
        }
    }

    /// Replace one collected single-link regular file from bounded in-memory
    /// bytes. The collector owns publication and witness updates; callers
    /// never write through the mutable output-tree pathname, and no caller-
    /// supplied reader can block while the mutation transaction is held.
    #[allow(dead_code)]
    pub(crate) fn replace_regular_from(&mut self, replacement: &[u8]) -> Result<(), Error> {
        mutation::replace_regular_from(self, replacement)
    }

    pub(crate) fn check_deadline(&self) -> Result<(), Error> {
        self.verified
            .as_ref()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .deadline
            .check(&self.path)
    }

    pub(crate) fn remaining_time(&self) -> Result<Duration, Error> {
        self.verified
            .as_ref()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .deadline
            .remaining(&self.path)
    }

    pub(crate) fn regular_file_byte_limit(&self) -> Result<u64, Error> {
        Ok(self
            .verified
            .as_ref()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .limits
            .max_file_bytes)
    }

    pub(crate) fn inventory_contains_regular_target(&self, target: &Path) -> Result<bool, Error> {
        let verified = self.verified.as_ref().ok_or_else(|| Error::UnverifiedContent {
            path: self.path.clone(),
        })?;
        verified.deadline.check(target)?;
        let relative = target.strip_prefix("/").unwrap_or(target);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Ok(false);
        }
        verified.witness.contains_regular(relative)
    }

    pub(crate) fn inventory_contains_symlink_target(&self, target: &Path) -> Result<bool, Error> {
        let verified = self.verified.as_ref().ok_or_else(|| Error::UnverifiedContent {
            path: self.path.clone(),
        })?;
        verified.deadline.check(target)?;
        let relative = target.strip_prefix("/").unwrap_or(target);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Ok(false);
        }
        verified.witness.contains_symlink(relative)
    }

    pub(crate) fn symlink_target(&self) -> Result<&str, Error> {
        let verified = self.verified.as_ref().ok_or_else(|| Error::UnverifiedContent {
            path: self.path.clone(),
        })?;
        let VerifiedKind::Symlink { target } = &verified.kind else {
            return Err(Error::UnverifiedContent {
                path: self.path.clone(),
            });
        };
        verified.verify()?;
        Ok(target)
    }

    pub(crate) fn file_times(&self) -> Result<(SystemTime, SystemTime), Error> {
        let verified = self.verified.as_ref().ok_or_else(|| Error::UnverifiedContent {
            path: self.path.clone(),
        })?;
        verified.verify()?;
        let path = verified.display_path()?;
        let parent = verified.open_parent("open collected entry for timestamp capture")?;
        let handle = open_entry_handle(&parent, &verified.name, &path)?;
        let current = metadata(&handle, "capture collected entry timestamps", &path)?;
        require_snapshot(&path, verified.snapshot, &current)?;
        let accessed = current.accessed().map_err(|source| Error::Io {
            operation: "read collected entry access time",
            path: path.clone(),
            source,
        })?;
        let modified = current.modified().map_err(|source| Error::Io {
            operation: "read collected entry modification time",
            path: path.clone(),
            source,
        })?;
        let reopened = open_entry_handle(&parent, &verified.name, &path)?;
        require_snapshot(
            &path,
            verified.snapshot,
            &metadata(&reopened, "verify collected entry after timestamp capture", &path)?,
        )?;
        verified.deadline.check(&path)?;
        Ok((accessed, modified))
    }

    pub(crate) fn verify_unchanged(&self) -> Result<(), Error> {
        self.verified
            .as_ref()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .verify()
    }

    pub(crate) fn open_verified(&self) -> Result<VerifiedFileReader, Error> {
        self.verified
            .as_ref()
            .ok_or_else(|| Error::UnverifiedContent {
                path: self.path.clone(),
            })?
            .open_regular()
    }

    pub fn is_file(&self) -> bool {
        matches!(self.layout.file, StonePayloadLayoutFile::Regular(..))
    }

    pub fn is_symlink(&self) -> bool {
        matches!(self.layout.file, StonePayloadLayoutFile::Symlink(..))
    }

    pub fn file_hash(&self) -> Option<u128> {
        if let StonePayloadLayoutFile::Regular(hash, _) = &self.layout.file {
            Some(*hash)
        } else {
            None
        }
    }

    pub fn file_name(&self) -> &str {
        self.target_path
            .file_name()
            .and_then(|path| path.to_str())
            .unwrap_or_default()
    }

    pub fn has_component(&self, component: &str) -> bool {
        self.target_path
            .components()
            .any(|path| path.as_os_str() == OsStr::new(component))
    }
}

pub(crate) struct VerifiedFileReader {
    file: File,
    parent: File,
    anchor: Arc<RootAnchor>,
    witness: Arc<WitnessGraph>,
    parent_id: DirectoryId,
    parent_identity: NodeIdentity,
    name: OsString,
    path: PathBuf,
    expected: FileSnapshot,
    expected_hash: u128,
    hasher: StoneDigestWriterHasher,
    bytes: u64,
    deadline: Arc<Deadline>,
    exceeded: bool,
}

impl VerifiedFileReader {
    pub(crate) fn finish(self) -> Result<(), Error> {
        self.deadline.check(&self.path)?;
        if self.exceeded || self.bytes != self.expected.size {
            return Err(Error::ContentLengthChanged {
                path: self.path,
                expected: self.expected.size,
                actual: self.bytes.saturating_add(u64::from(self.exceeded)),
            });
        }
        let actual_hash = self.hasher.digest128();
        if actual_hash != self.expected_hash {
            return Err(Error::ContentHashChanged {
                path: self.path,
                expected: self.expected_hash,
                actual: actual_hash,
            });
        }
        require_snapshot(
            &self.path,
            self.expected,
            &metadata(&self.file, "verify emitted package file", &self.path)?,
        )?;
        if NodeIdentity::from_metadata(&metadata(&self.parent, "verify emitted package parent", &self.path)?)
            != self.parent_identity
        {
            return Err(changed(&self.path, "package file parent changed during emission"));
        }
        self.anchor.verify_path_node()?;
        let parent = self.witness.open_directory(self.parent_id, &self.path)?;
        if NodeIdentity::from_metadata(&metadata(&parent, "reopen emitted package parent", &self.path)?)
            != self.parent_identity
        {
            return Err(changed(&self.path, "package file parent was replaced during emission"));
        }
        let reopened = open_entry_handle(&parent, &self.name, &self.path)?;
        require_snapshot(
            &self.path,
            self.expected,
            &metadata(&reopened, "reopen emitted package file", &self.path)?,
        )
    }
}

impl Read for VerifiedFileReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if let Err(error) = self.deadline.check(&self.path) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, error));
        }
        if self.bytes == self.expected.size {
            let mut probe = [0u8; 1];
            return match self.file.read(&mut probe)? {
                0 => Ok(0),
                _ => {
                    self.exceeded = true;
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "package file grew during verified emission",
                    ))
                }
            };
        }

        let remaining = self.expected.size - self.bytes;
        let allowed = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let read = self.file.read(&mut buffer[..allowed])?;
        if read != 0 {
            self.bytes = self
                .bytes
                .checked_add(read as u64)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "package byte count overflow"))?;
            self.hasher.update(&buffer[..read]);
        }
        Ok(read)
    }
}

pub(super) fn layout_from_metadata(
    relative: &Path,
    metadata: &Metadata,
    symlink_target: Option<&str>,
    regular_hash: Option<u128>,
) -> Result<StonePayloadLayoutRecord, Error> {
    let full_target_path = Path::new("/").join(relative);
    let target_path = full_target_path.strip_prefix("/usr").unwrap_or(&full_target_path);
    let target: AStr = target_path
        .to_str()
        .ok_or_else(|| Error::NonUtf8Path {
            path: Path::new("/").join(relative),
        })?
        .into();
    let file_type = metadata.file_type();
    let file = if file_type.is_symlink() {
        let source = symlink_target.ok_or_else(|| Error::TreeChanged {
            path: full_target_path.clone(),
            detail: "symlink target was not captured",
        })?;
        StonePayloadLayoutFile::Symlink(source.into(), target)
    } else if file_type.is_dir() {
        StonePayloadLayoutFile::Directory(target)
    } else if file_type.is_char_device() {
        StonePayloadLayoutFile::CharacterDevice(target)
    } else if file_type.is_block_device() {
        StonePayloadLayoutFile::BlockDevice(target)
    } else if file_type.is_fifo() {
        StonePayloadLayoutFile::Fifo(target)
    } else if file_type.is_socket() {
        StonePayloadLayoutFile::Socket(target)
    } else if file_type.is_file() {
        StonePayloadLayoutFile::Regular(
            regular_hash.ok_or_else(|| Error::TreeChanged {
                path: full_target_path.clone(),
                detail: "regular file hash was not captured",
            })?,
            target,
        )
    } else {
        return Err(Error::UnsupportedFileType {
            path: full_target_path,
            kind: "unknown special inode",
        });
    };

    Ok(StonePayloadLayoutRecord {
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.mode(),
        tag: 0,
        file,
    })
}
