// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::{
    ffi::{CStr, CString, OsStr},
    fs::Metadata,
    io::{self, Read, Seek, SeekFrom, Write},
    mem::{size_of, zeroed},
    num::NonZeroU64,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    ptr::NonNull,
    time::Duration,
};

use forge::package::{Meta, is_reserved_usr_layout_target};
use fs_err::File;
use itertools::Itertools;
use nix::{errno::Errno, libc};
use regex::Regex;
use sha2::{Digest, Sha256};
use snafu::{ResultExt, Snafu};
use stone::{
    StoneHeaderV1FileType, StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag, StoneWriteError,
    StoneWriter,
    relation::{Dependency, Provider},
};
use stone_recipe::derivation::{DerivationId, PackageIdentity};
use tui::{ProgressBar, ProgressStyle, Styled};

use self::manifest::Manifest;
use super::{ResolvedOutput, analysis, collect};
use crate::{Architecture, Paths};

mod manifest;

const RECIPE_FINGERPRINT_SOURCE_REF_PREFIX: &str = "gluon-evaluation-sha256:";
const DERIVATION_ID_SOURCE_REF_PREFIX: &str = "derivation-sha256:";
const EMISSION_STAGE_NAME: &[u8] = b".mason-emission";
const EMISSION_SCRATCH_NAME: &[u8] = b".content-scratch";
const MAX_EMITTED_ARTIFACTS: usize = 256;
const MAX_STONE_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024 * 1024;
const MAX_MANIFEST_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const ARTIFACT_DIGEST_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
struct ArtifactSpec {
    name: String,
    max_bytes: u64,
}

impl ArtifactSpec {
    fn stone(name: String) -> Self {
        Self {
            name,
            max_bytes: MAX_STONE_ARTIFACT_BYTES,
        }
    }

    fn manifest(name: String) -> Self {
        Self {
            name,
            max_bytes: MAX_MANIFEST_ARTIFACT_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryStamp {
    identity: Identity,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileWitness {
    identity: Identity,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileWitness {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            identity: Identity::from_metadata(metadata),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    fn unchanged_by_rename_from(self, previous: Self) -> bool {
        self.identity == previous.identity
            && self.mode == previous.mode
            && self.links == previous.links
            && self.length == previous.length
            && self.modified_seconds == previous.modified_seconds
            && self.modified_nanoseconds == previous.modified_nanoseconds
    }
}

impl DirectoryStamp {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            identity: Identity::from_metadata(metadata),
            mode: metadata.mode(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct DirectoryHandle {
    path: PathBuf,
    file: File,
    identity: Identity,
}

impl DirectoryHandle {
    fn open_root(path: &Path) -> Result<Self, ArtifactError> {
        let path = std::path::absolute(path).map_err(|source| ArtifactError::Io {
            operation: "make artifact root absolute",
            path: path.to_owned(),
            source,
        })?;
        let file = openat2_file(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            &path,
        )
        .map_err(|source| ArtifactError::Io {
            operation: "open artifact root without symlinks",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| ArtifactError::Io {
            operation: "inspect opened artifact root",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(ArtifactError::UnexpectedKind {
                role: "artifact root",
                path,
                expected: "directory",
            });
        }
        Ok(Self {
            path,
            identity: Identity::from_metadata(&metadata),
            file,
        })
    }

    fn open_child_directory(&self, name: &[u8]) -> Result<Self, ArtifactError> {
        let path = self.display(name);
        let file = openat2_file(
            self.file.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
            &path,
        )
        .map_err(|source| ArtifactError::Io {
            operation: "open artifact staging directory",
            path: path.clone(),
            source,
        })?;
        let metadata = file.metadata().map_err(|source| ArtifactError::Io {
            operation: "inspect artifact staging directory",
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(ArtifactError::UnexpectedKind {
                role: "artifact staging entry",
                path,
                expected: "directory",
            });
        }
        Ok(Self {
            path,
            identity: Identity::from_metadata(&metadata),
            file,
        })
    }

    fn display(&self, name: &[u8]) -> PathBuf {
        self.path.join(OsStr::from_bytes(name))
    }

    fn metadata(&self, operation: &'static str) -> Result<Metadata, ArtifactError> {
        self.file.metadata().map_err(|source| ArtifactError::Io {
            operation,
            path: self.path.clone(),
            source,
        })
    }

    fn require_path_identity(&self) -> Result<(), ArtifactError> {
        let reopened = openat2_file(
            libc::AT_FDCWD,
            self.path.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            &self.path,
        )
        .map_err(|source| ArtifactError::Io {
            operation: "reopen public artifact root",
            path: self.path.clone(),
            source,
        })?;
        let metadata = reopened.metadata().map_err(|source| ArtifactError::Io {
            operation: "inspect public artifact root",
            path: self.path.clone(),
            source,
        })?;
        if !metadata.file_type().is_dir() || Identity::from_metadata(&metadata) != self.identity {
            return Err(ArtifactError::OwnershipChanged {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    fn inspect(&self, name: &[u8], operation: &'static str) -> Result<Option<(Metadata, Identity)>, ArtifactError> {
        let path = self.display(name);
        match openat2_file(
            self.file.as_raw_fd(),
            name,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
            descendant_resolution(),
            &path,
        ) {
            Ok(file) => {
                let metadata = file.metadata().map_err(|source| ArtifactError::Io {
                    operation,
                    path,
                    source,
                })?;
                let identity = Identity::from_metadata(&metadata);
                Ok(Some((metadata, identity)))
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ArtifactError::Io {
                operation,
                path,
                source,
            }),
        }
    }

    fn require_inventory(&self, role: &'static str, expected: &[Vec<u8>]) -> Result<(), ArtifactError> {
        let maximum = expected.len().checked_add(1).ok_or(ArtifactError::ResourceLimit {
            resource: "artifact directory entries",
            limit: expected.len(),
        })?;
        let before = DirectoryStamp::from_metadata(&self.metadata("inspect artifact directory before enumeration")?);
        let first = self.read_names(maximum)?;
        let between = DirectoryStamp::from_metadata(&self.metadata("inspect artifact directory after enumeration")?);
        if before != between {
            return Err(ArtifactError::DirectoryChanged {
                path: self.path.clone(),
            });
        }
        let second = self.read_names(maximum)?;
        let after = DirectoryStamp::from_metadata(&self.metadata("confirm artifact directory enumeration")?);
        if between != after || first != second {
            return Err(ArtifactError::DirectoryChanged {
                path: self.path.clone(),
            });
        }
        if first != expected {
            return Err(ArtifactError::InventoryMismatch {
                role,
                path: self.path.clone(),
                expected: copy_name_list(expected, "expected artifact inventory names")?,
                found: first,
            });
        }
        Ok(())
    }

    fn read_names(&self, maximum: usize) -> Result<Vec<Vec<u8>>, ArtifactError> {
        let cursor = openat2_file(
            self.file.as_raw_fd(),
            b".",
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
            descendant_resolution(),
            &self.path,
        )
        .map_err(|source| ArtifactError::Io {
            operation: "open fresh artifact directory cursor",
            path: self.path.clone(),
            source,
        })?;
        let stream = DirectoryStream::from_file(cursor, &self.path)?;
        let mut names = Vec::new();
        names.try_reserve(maximum).map_err(|source| ArtifactError::Allocation {
            resource: "artifact directory names",
            requested: maximum,
            detail: source.to_string(),
        })?;
        loop {
            Errno::clear();
            // SAFETY: the DIR pointer is live and exclusively borrowed for
            // this iteration. readdir returns storage owned by that stream.
            let entry = unsafe { libc::readdir(stream.0.as_ptr()) };
            if entry.is_null() {
                let error = Errno::last();
                if error == Errno::UnknownErrno {
                    break;
                }
                return Err(ArtifactError::Io {
                    operation: "enumerate artifact directory",
                    path: self.path.clone(),
                    source: io::Error::from_raw_os_error(error as i32),
                });
            }
            // SAFETY: d_name is NUL-terminated and remains live until the next
            // operation on this directory stream.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            if names.len() == maximum {
                return Err(ArtifactError::ResourceLimit {
                    resource: "artifact directory entries",
                    limit: maximum,
                });
            }
            names.push(copy_bytes(name, "artifact directory entry name")?);
        }
        names.sort_unstable();
        Ok(names)
    }
}

struct DirectoryStream(NonNull<libc::DIR>);

impl DirectoryStream {
    fn from_file(file: File, path: &Path) -> Result<Self, ArtifactError> {
        let descriptor = file.into_raw_fd();
        // SAFETY: descriptor is a fresh owned directory descriptor. fdopendir
        // consumes it on success and leaves ownership with us on failure.
        let stream = unsafe { libc::fdopendir(descriptor) };
        match NonNull::new(stream) {
            Some(stream) => Ok(Self(stream)),
            None => {
                let source = io::Error::last_os_error();
                // SAFETY: fdopendir failed and did not consume descriptor.
                unsafe { libc::close(descriptor) };
                Err(ArtifactError::Io {
                    operation: "open artifact directory stream",
                    path: path.to_owned(),
                    source,
                })
            }
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the stream returned by fdopendir.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

#[derive(Debug)]
struct BoundedFile {
    file: File,
    path: PathBuf,
    maximum: u64,
    position: u64,
    length: u64,
}

impl BoundedFile {
    fn new(file: File, path: PathBuf, maximum: u64) -> Self {
        Self {
            file,
            path,
            maximum,
            position: 0,
            length: 0,
        }
    }

    fn reset(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.position = 0;
        self.length = 0;
        Ok(())
    }

    fn seek_target(&self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if !(0..=i128::from(self.maximum)).contains(&target) {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!(
                    "artifact seek would leave the 0..={} byte range for {}",
                    self.maximum,
                    self.path.display()
                ),
            ));
        }
        Ok(target as u64)
    }
}

impl Write for BoundedFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.position);
        if remaining == 0 && !buffer.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!(
                    "artifact write exceeds {} byte limit for {}",
                    self.maximum,
                    self.path.display()
                ),
            ));
        }
        let allowed = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let written = self.file.write(&buffer[..allowed])?;
        self.position = self
            .position
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("artifact write counter overflow"))?;
        self.length = self.length.max(self.position);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Read for BoundedFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.position);
        let allowed = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let read = self.file.read(&mut buffer[..allowed])?;
        self.position = self
            .position
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("artifact read counter overflow"))?;
        Ok(read)
    }
}

impl Seek for BoundedFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = self.seek_target(position)?;
        let actual = self.file.seek(SeekFrom::Start(target))?;
        if actual != target {
            return Err(io::Error::other("artifact descriptor sought to an unexpected offset"));
        }
        self.position = actual;
        Ok(actual)
    }
}

#[derive(Debug)]
struct ArtifactSlot {
    final_name: Vec<u8>,
    stage_name: Vec<u8>,
    identity: Identity,
    file: BoundedFile,
    activated: bool,
    sealed: bool,
    witness: Option<FileWitness>,
    digest: Option<[u8; 32]>,
    published: bool,
    cleaned: bool,
}

#[derive(Debug)]
struct ScratchSlot {
    identity: Identity,
    file: BoundedFile,
}

#[derive(Debug)]
struct ArtifactSink {
    root: DirectoryHandle,
    stage: DirectoryHandle,
    slots: Vec<ArtifactSlot>,
    scratch: Option<ScratchSlot>,
    stage_removed: bool,
    active: bool,
}

impl ArtifactSink {
    fn new(root_path: &Path, mut specs: Vec<ArtifactSpec>) -> Result<Self, ArtifactError> {
        if specs.len() > MAX_EMITTED_ARTIFACTS {
            return Err(ArtifactError::ResourceLimit {
                resource: "emitted artifacts",
                limit: MAX_EMITTED_ARTIFACTS,
            });
        }
        specs.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        for spec in &specs {
            validate_artifact_name(&spec.name)?;
        }
        for pair in specs.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(ArtifactError::DuplicateName {
                    name: pair[0].name.clone(),
                });
            }
        }

        let root = DirectoryHandle::open_root(root_path)?;
        root.require_inventory("initial artifact root", &[])?;
        let stage_path = root.display(EMISSION_STAGE_NAME);
        let stage_name = c_name(EMISSION_STAGE_NAME, &stage_path)?;
        // SAFETY: root and stage_name remain live, and mkdirat interprets the
        // NUL-terminated name relative to the pinned root descriptor.
        if unsafe { libc::mkdirat(root.file.as_raw_fd(), stage_name.as_ptr(), 0o700) } == -1 {
            return Err(ArtifactError::Io {
                operation: "create private artifact staging directory",
                path: stage_path,
                source: io::Error::last_os_error(),
            });
        }
        let stage_pin = match openat2_file(
            root.file.as_raw_fd(),
            EMISSION_STAGE_NAME,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
            descendant_resolution(),
            &stage_path,
        ) {
            Ok(stage_pin) => stage_pin,
            Err(source) => {
                let primary = ArtifactError::Io {
                    operation: "pin newly created artifact staging directory",
                    path: stage_path.clone(),
                    source,
                };
                return Err(ArtifactError::Rollback {
                    primary: Box::new(primary),
                    cleanup: Box::new(ArtifactError::UnprovenCleanup { path: stage_path }),
                });
            }
        };
        let stage_metadata = stage_pin.metadata().map_err(|source| ArtifactError::Rollback {
            primary: Box::new(ArtifactError::Io {
                operation: "inspect newly pinned artifact staging directory",
                path: stage_path.clone(),
                source,
            }),
            cleanup: Box::new(ArtifactError::UnprovenCleanup {
                path: stage_path.clone(),
            }),
        })?;
        let stage_identity = Identity::from_metadata(&stage_metadata);
        let stage = match root.open_child_directory(EMISSION_STAGE_NAME) {
            Ok(stage) if stage.identity == stage_identity => stage,
            Ok(_) => {
                let primary = ArtifactError::OwnershipChanged {
                    path: stage_path.clone(),
                };
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(
                        &root,
                        EMISSION_STAGE_NAME,
                        stage_identity,
                        true,
                        false,
                        "remove replaced artifact staging directory",
                    ),
                ));
            }
            Err(primary) => {
                return Err(with_cleanup(
                    primary,
                    remove_owned_entry(
                        &root,
                        EMISSION_STAGE_NAME,
                        stage_identity,
                        true,
                        false,
                        "remove artifact staging directory after open failure",
                    ),
                ));
            }
        };
        let mut sink = Self {
            root,
            stage,
            slots: Vec::new(),
            scratch: None,
            stage_removed: false,
            active: true,
        };

        let setup = sink.populate(specs);
        if let Err(primary) = setup {
            return match sink.abort() {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(ArtifactError::Rollback {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        Ok(sink)
    }

    fn populate(&mut self, specs: Vec<ArtifactSpec>) -> Result<(), ArtifactError> {
        // SAFETY: stage is a live descriptor for the directory created above;
        // this removes any ambient umask influence from its final mode.
        if unsafe { libc::fchmod(self.stage.file.as_raw_fd(), 0o700) } == -1 {
            return Err(ArtifactError::Io {
                operation: "normalize private artifact staging directory mode",
                path: self.stage.path.clone(),
                source: io::Error::last_os_error(),
            });
        }
        self.slots
            .try_reserve(specs.len())
            .map_err(|source| ArtifactError::Allocation {
                resource: "artifact staging slots",
                requested: specs.len(),
                detail: source.to_string(),
            })?;
        for (index, spec) in specs.into_iter().enumerate() {
            let stage_name = format!("artifact-{index:03}").into_bytes();
            let path = self.stage.display(&stage_name);
            let (file, metadata) = create_owned_file(&self.stage, &stage_name, &path)?;
            self.slots.push(ArtifactSlot {
                final_name: spec.name.into_bytes(),
                stage_name,
                identity: Identity::from_metadata(&metadata),
                file: BoundedFile::new(file, path, spec.max_bytes),
                activated: false,
                sealed: false,
                witness: None,
                digest: None,
                published: false,
                cleaned: false,
            });
        }

        let scratch_path = self.stage.display(EMISSION_SCRATCH_NAME);
        let (scratch_file, scratch_metadata) = create_owned_file(&self.stage, EMISSION_SCRATCH_NAME, &scratch_path)?;
        self.scratch = Some(ScratchSlot {
            identity: Identity::from_metadata(&scratch_metadata),
            file: BoundedFile::new(scratch_file, scratch_path, MAX_STONE_ARTIFACT_BYTES),
        });

        let root_inventory = one_name(EMISSION_STAGE_NAME)?;
        self.root
            .require_inventory("artifact root during staging", &root_inventory)?;
        self.require_stage_inventory(true)?;
        self.require_stage_identity()?;
        Ok(())
    }

    fn writer(&mut self, final_name: &str) -> Result<&mut BoundedFile, ArtifactError> {
        let index = self.slot_index(final_name)?;
        let slot = &mut self.slots[index];
        if slot.activated {
            return Err(ArtifactError::AlreadyPrepared {
                name: final_name.to_owned(),
            });
        }
        slot.activated = true;
        Ok(&mut slot.file)
    }

    fn package_writers(&mut self, final_name: &str) -> Result<(&mut BoundedFile, &mut BoundedFile), ArtifactError> {
        let index = self.slot_index(final_name)?;
        self.require_slot_identity(index, false)?;
        self.require_scratch_identity()?;
        let (slots, scratch) = (&mut self.slots, &mut self.scratch);
        let slot = &mut slots[index];
        if slot.activated {
            return Err(ArtifactError::AlreadyPrepared {
                name: final_name.to_owned(),
            });
        }
        slot.activated = true;
        let scratch = scratch.as_mut().ok_or(ArtifactError::ScratchUnavailable)?;
        scratch.file.reset().map_err(|source| ArtifactError::Io {
            operation: "reset bounded content scratch",
            path: scratch.file.path.clone(),
            source,
        })?;
        Ok((&mut slot.file, &mut scratch.file))
    }

    fn slot_index(&self, final_name: &str) -> Result<usize, ArtifactError> {
        self.slots
            .binary_search_by(|slot| slot.final_name.as_slice().cmp(final_name.as_bytes()))
            .map_err(|_| ArtifactError::UnexpectedName {
                name: final_name.to_owned(),
            })
    }

    fn commit(&mut self) -> Result<(), ArtifactError> {
        self.commit_with_hooks(|_, _| {}, |_, _| {})
    }

    #[cfg(test)]
    fn commit_with_hook<F>(&mut self, hook: F) -> Result<(), ArtifactError>
    where
        F: FnMut(usize, &Path),
    {
        self.commit_with_hooks(|_, _| {}, hook)
    }

    fn commit_with_hooks<B, A>(&mut self, before_rename: B, after_rename: A) -> Result<(), ArtifactError>
    where
        B: FnMut(usize, &Path),
        A: FnMut(usize, &Path),
    {
        let result = self.commit_inner(before_rename, after_rename);
        if let Err(primary) = result {
            return match self.abort() {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(ArtifactError::Rollback {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        self.active = false;
        Ok(())
    }

    fn commit_inner<B, A>(&mut self, mut before_rename: B, mut after_rename: A) -> Result<(), ArtifactError>
    where
        B: FnMut(usize, &Path),
        A: FnMut(usize, &Path),
    {
        self.root.require_path_identity()?;
        self.require_stage_identity()?;
        let root_inventory = one_name(EMISSION_STAGE_NAME)?;
        self.root
            .require_inventory("artifact root before publication", &root_inventory)?;
        self.require_stage_inventory(true)?;

        for index in 0..self.slots.len() {
            self.seal_slot(index)?;
        }
        self.remove_scratch()?;
        self.require_stage_inventory(false)?;
        self.stage.file.sync_all().map_err(|source| ArtifactError::Io {
            operation: "sync artifact staging directory",
            path: self.stage.path.clone(),
            source,
        })?;
        self.require_stage_identity()?;

        for index in 0..self.slots.len() {
            let staged_path = self.stage.display(&self.slots[index].stage_name);
            self.require_slot_integrity(index, false)?;
            // Test-only fault injection lives after the final staged check so
            // the following publication refresh must independently prove the
            // retained inode's bytes, not merely inherit that check.
            before_rename(index, &staged_path);
            rename_noreplace_at(
                &self.stage,
                &self.slots[index].stage_name,
                &self.root,
                &self.slots[index].final_name,
            )?;
            self.slots[index].published = true;
            self.refresh_published_witness(index)?;
            let path = self.root.display(&self.slots[index].final_name);
            after_rename(index, &path);
            self.require_slot_integrity(index, true)?;
        }

        self.stage
            .require_inventory("drained artifact staging directory", &[])?;
        self.stage.file.sync_all().map_err(|source| ArtifactError::Io {
            operation: "sync drained artifact staging directory",
            path: self.stage.path.clone(),
            source,
        })?;
        remove_owned_entry(
            &self.root,
            EMISSION_STAGE_NAME,
            self.stage.identity,
            true,
            false,
            "remove drained artifact staging directory",
        )?;
        self.stage_removed = true;

        let final_names = self.final_names()?;
        self.root.require_inventory("published artifact root", &final_names)?;
        for index in 0..self.slots.len() {
            self.require_slot_identity(index, true)?;
        }
        self.root.require_path_identity()?;
        self.root.file.sync_all().map_err(|source| ArtifactError::Io {
            operation: "sync published artifact root",
            path: self.root.path.clone(),
            source,
        })?;

        // Recheck after the durability barrier. This catches a concurrent
        // same-name replacement before the held output capabilities close.
        self.root.require_inventory("confirmed artifact root", &final_names)?;
        for index in 0..self.slots.len() {
            self.require_slot_integrity(index, true)?;
        }
        self.root.require_path_identity()
    }

    fn seal_slot(&mut self, index: usize) -> Result<(), ArtifactError> {
        if !self.slots[index].activated {
            return Err(ArtifactError::NotPrepared {
                name: String::from_utf8_lossy(&self.slots[index].final_name).into_owned(),
            });
        }
        self.require_slot_identity(index, false)?;
        let slot = &mut self.slots[index];
        slot.file.flush().map_err(|source| ArtifactError::Io {
            operation: "flush staged artifact",
            path: slot.file.path.clone(),
            source,
        })?;
        // SAFETY: the descriptor is live; fchmod changes only this owned inode.
        if unsafe { libc::fchmod(slot.file.file.as_raw_fd(), 0o444) } == -1 {
            return Err(ArtifactError::Io {
                operation: "make staged artifact read-only",
                path: slot.file.path.clone(),
                source: io::Error::last_os_error(),
            });
        }
        slot.file.file.sync_all().map_err(|source| ArtifactError::Io {
            operation: "sync staged artifact",
            path: slot.file.path.clone(),
            source,
        })?;
        let metadata = slot.file.file.metadata().map_err(|source| ArtifactError::Io {
            operation: "inspect sealed staged artifact",
            path: slot.file.path.clone(),
            source,
        })?;
        require_regular_witness(&slot.file.path, &metadata, slot.identity, 0o444, slot.file.maximum)?;
        let witness = FileWitness::from_metadata(&metadata);
        let digest = digest_descriptor(&slot.file.file, &slot.file.path, witness)?;
        slot.witness = Some(witness);
        slot.digest = Some(digest);
        slot.sealed = true;
        self.require_slot_integrity(index, false)
    }

    fn refresh_published_witness(&mut self, index: usize) -> Result<(), ArtifactError> {
        let slot = &self.slots[index];
        let previous = slot.witness.ok_or_else(|| ArtifactError::ArtifactChanged {
            path: slot.file.path.clone(),
        })?;
        let path = self.root.display(&slot.final_name);
        let Some((named_metadata, identity)) = self
            .root
            .inspect(&slot.final_name, "authenticate newly published artifact")?
        else {
            return Err(ArtifactError::OwnershipChanged { path });
        };
        let descriptor_metadata = slot.file.file.metadata().map_err(|source| ArtifactError::Io {
            operation: "inspect retained published artifact descriptor",
            path: path.clone(),
            source,
        })?;
        require_regular_witness(&path, &named_metadata, slot.identity, 0o444, slot.file.maximum)?;
        require_regular_witness(&path, &descriptor_metadata, slot.identity, 0o444, slot.file.maximum)?;
        let named_witness = FileWitness::from_metadata(&named_metadata);
        if identity != slot.identity
            || named_witness != FileWitness::from_metadata(&descriptor_metadata)
            || !named_witness.unchanged_by_rename_from(previous)
        {
            return Err(ArtifactError::ArtifactChanged { path });
        }
        let expected_digest = slot
            .digest
            .ok_or_else(|| ArtifactError::ArtifactChanged { path: path.clone() })?;
        let found_digest = digest_descriptor(&slot.file.file, &path, named_witness)?;
        if found_digest != expected_digest {
            return Err(ArtifactError::DigestChanged { path });
        }
        self.slots[index].witness = Some(named_witness);
        Ok(())
    }

    fn require_slot_integrity(&self, index: usize, published: bool) -> Result<(), ArtifactError> {
        self.require_slot_identity(index, published)?;
        let slot = &self.slots[index];
        let (directory, name) = if published {
            (&self.root, slot.final_name.as_slice())
        } else {
            (&self.stage, slot.stage_name.as_slice())
        };
        let path = directory.display(name);
        let witness = slot
            .witness
            .ok_or_else(|| ArtifactError::ArtifactChanged { path: path.clone() })?;
        let expected_digest = slot
            .digest
            .ok_or_else(|| ArtifactError::ArtifactChanged { path: path.clone() })?;
        let found_digest = digest_descriptor(&slot.file.file, &path, witness)?;
        if found_digest != expected_digest {
            return Err(ArtifactError::DigestChanged { path });
        }
        Ok(())
    }

    fn require_slot_identity(&self, index: usize, published: bool) -> Result<(), ArtifactError> {
        let slot = &self.slots[index];
        let (directory, name, role) = if published {
            (&self.root, slot.final_name.as_slice(), "published artifact")
        } else {
            (&self.stage, slot.stage_name.as_slice(), "staged artifact")
        };
        let path = directory.display(name);
        let Some((metadata, identity)) = directory.inspect(name, "reopen owned artifact")? else {
            return Err(ArtifactError::OwnershipChanged { path });
        };
        if identity != slot.identity || !metadata.file_type().is_file() || metadata.nlink() != 1 {
            return Err(ArtifactError::OwnershipChanged { path });
        }
        if slot.sealed {
            require_regular_witness(&path, &metadata, slot.identity, 0o444, slot.file.maximum)?;
            let witness = slot
                .witness
                .ok_or_else(|| ArtifactError::ArtifactChanged { path: path.clone() })?;
            if FileWitness::from_metadata(&metadata) != witness {
                return Err(ArtifactError::ArtifactChanged { path });
            }
        } else if metadata.mode() & 0o7777 != 0o600 {
            return Err(ArtifactError::ModeMismatch {
                role,
                path,
                expected: 0o600,
                found: metadata.mode() & 0o7777,
            });
        }
        Ok(())
    }

    fn require_scratch_identity(&self) -> Result<(), ArtifactError> {
        let scratch = self.scratch.as_ref().ok_or(ArtifactError::ScratchUnavailable)?;
        let path = self.stage.display(EMISSION_SCRATCH_NAME);
        let Some((metadata, identity)) = self.stage.inspect(EMISSION_SCRATCH_NAME, "reopen content scratch")? else {
            return Err(ArtifactError::OwnershipChanged { path });
        };
        if identity != scratch.identity
            || !metadata.file_type().is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o7777 != 0o600
        {
            return Err(ArtifactError::OwnershipChanged { path });
        }
        Ok(())
    }

    fn require_stage_identity(&self) -> Result<(), ArtifactError> {
        let path = self.root.display(EMISSION_STAGE_NAME);
        let Some((metadata, identity)) = self
            .root
            .inspect(EMISSION_STAGE_NAME, "reopen artifact staging directory")?
        else {
            return Err(ArtifactError::OwnershipChanged { path });
        };
        if identity != self.stage.identity || !metadata.file_type().is_dir() || metadata.mode() & 0o7777 != 0o700 {
            return Err(ArtifactError::OwnershipChanged { path });
        }
        Ok(())
    }

    fn require_stage_inventory(&self, include_scratch: bool) -> Result<(), ArtifactError> {
        let capacity =
            self.slots
                .len()
                .checked_add(usize::from(include_scratch))
                .ok_or(ArtifactError::ResourceLimit {
                    resource: "artifact staging inventory entries",
                    limit: MAX_EMITTED_ARTIFACTS + 1,
                })?;
        let mut names = Vec::new();
        names
            .try_reserve_exact(capacity)
            .map_err(|source| ArtifactError::Allocation {
                resource: "artifact staging inventory entries",
                requested: capacity,
                detail: source.to_string(),
            })?;
        for slot in &self.slots {
            names.push(copy_bytes(&slot.stage_name, "artifact staging inventory name")?);
        }
        if include_scratch {
            names.push(copy_bytes(EMISSION_SCRATCH_NAME, "artifact scratch inventory name")?);
        }
        names.sort_unstable();
        self.stage.require_inventory("artifact staging directory", &names)
    }

    fn final_names(&self) -> Result<Vec<Vec<u8>>, ArtifactError> {
        let mut names = Vec::new();
        names
            .try_reserve_exact(self.slots.len())
            .map_err(|source| ArtifactError::Allocation {
                resource: "published artifact inventory entries",
                requested: self.slots.len(),
                detail: source.to_string(),
            })?;
        for slot in &self.slots {
            names.push(copy_bytes(&slot.final_name, "published artifact inventory name")?);
        }
        Ok(names)
    }

    fn remove_scratch(&mut self) -> Result<(), ArtifactError> {
        self.require_scratch_identity()?;
        let identity = self.scratch.as_ref().ok_or(ArtifactError::ScratchUnavailable)?.identity;
        remove_owned_entry(
            &self.stage,
            EMISSION_SCRATCH_NAME,
            identity,
            false,
            false,
            "remove artifact content scratch",
        )?;
        self.scratch = None;
        Ok(())
    }

    fn abort(&mut self) -> Result<(), ArtifactError> {
        if !self.active {
            return Ok(());
        }
        let mut failures = Vec::new();
        for index in (0..self.slots.len()).rev() {
            if self.slots[index].cleaned {
                continue;
            }
            let removal = {
                let slot = &self.slots[index];
                let (directory, name, role) = if slot.published {
                    (
                        &self.root,
                        slot.final_name.as_slice(),
                        "remove owned published artifact",
                    )
                } else {
                    (&self.stage, slot.stage_name.as_slice(), "remove owned staged artifact")
                };
                remove_owned_entry(directory, name, slot.identity, false, false, role)
            };
            match removal {
                Ok(()) => self.slots[index].cleaned = true,
                Err(error) => failures.push(error.to_string()),
            }
        }
        if let Some(scratch) = self.scratch.as_ref() {
            match remove_owned_entry(
                &self.stage,
                EMISSION_SCRATCH_NAME,
                scratch.identity,
                false,
                false,
                "remove owned content scratch",
            ) {
                Ok(()) => self.scratch = None,
                Err(error) => failures.push(error.to_string()),
            }
        }
        if !self.stage_removed {
            match remove_owned_entry(
                &self.root,
                EMISSION_STAGE_NAME,
                self.stage.identity,
                true,
                false,
                "remove owned artifact staging directory",
            ) {
                Ok(()) => self.stage_removed = true,
                Err(error) => failures.push(error.to_string()),
            }
        }
        if let Err(source) = self.root.file.sync_all() {
            failures.push(
                ArtifactError::Io {
                    operation: "sync artifact root after rollback",
                    path: self.root.path.clone(),
                    source,
                }
                .to_string(),
            );
        }
        if failures.is_empty() {
            self.active = false;
            Ok(())
        } else {
            Err(ArtifactError::Cleanup { failures })
        }
    }
}

impl Drop for ArtifactSink {
    fn drop(&mut self) {
        let _ = self.abort();
    }
}

fn create_owned_file(directory: &DirectoryHandle, name: &[u8], path: &Path) -> Result<(File, Metadata), ArtifactError> {
    let file = openat2_file(
        directory.file.as_raw_fd(),
        name,
        libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL,
        0o600,
        descendant_resolution(),
        path,
    )
    .map_err(|source| ArtifactError::Io {
        operation: "create exclusive staged artifact",
        path: path.to_owned(),
        source,
    })?;
    let initial_metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(source) => {
            return Err(ArtifactError::Rollback {
                primary: Box::new(ArtifactError::Io {
                    operation: "inspect exclusively created staged artifact",
                    path: path.to_owned(),
                    source,
                }),
                cleanup: Box::new(ArtifactError::UnprovenCleanup { path: path.to_owned() }),
            });
        }
    };
    let identity = Identity::from_metadata(&initial_metadata);
    let normalized = normalize_new_regular(&file, path, identity);
    match normalized {
        Ok(metadata) => Ok((file, metadata)),
        Err(primary) => Err(with_cleanup(
            primary,
            remove_owned_entry(
                directory,
                name,
                identity,
                false,
                false,
                "remove rejected newly created staged artifact",
            ),
        )),
    }
}

fn normalize_new_regular(file: &File, path: &Path, identity: Identity) -> Result<Metadata, ArtifactError> {
    // SAFETY: the descriptor is live; fchmod affects only the newly created
    // inode, independent of the process umask.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } == -1 {
        return Err(ArtifactError::Io {
            operation: "normalize staged artifact mode",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let metadata = file.metadata().map_err(|source| ArtifactError::Io {
        operation: "inspect newly created staged artifact",
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
        || Identity::from_metadata(&metadata) != identity
    {
        return Err(ArtifactError::UnexpectedKind {
            role: "new staged artifact",
            path: path.to_owned(),
            expected: "single-link regular file with mode 0600",
        });
    }
    Ok(metadata)
}

fn with_cleanup(primary: ArtifactError, cleanup: Result<(), ArtifactError>) -> ArtifactError {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => ArtifactError::Rollback {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        },
    }
}

fn require_regular_witness(
    path: &Path,
    metadata: &Metadata,
    expected_identity: Identity,
    expected_mode: u32,
    maximum: u64,
) -> Result<(), ArtifactError> {
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || Identity::from_metadata(metadata) != expected_identity
    {
        return Err(ArtifactError::OwnershipChanged { path: path.to_owned() });
    }
    let mode = metadata.mode() & 0o7777;
    if mode != expected_mode {
        return Err(ArtifactError::ModeMismatch {
            role: "sealed artifact",
            path: path.to_owned(),
            expected: expected_mode,
            found: mode,
        });
    }
    if metadata.len() > maximum {
        return Err(ArtifactError::ArtifactTooLarge {
            path: path.to_owned(),
            maximum,
            found: metadata.len(),
        });
    }
    Ok(())
}

fn digest_descriptor(file: &File, path: &Path, expected: FileWitness) -> Result<[u8; 32], ArtifactError> {
    let before = file.metadata().map_err(|source| ArtifactError::Io {
        operation: "inspect sealed artifact before hashing",
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&before) != expected {
        return Err(ArtifactError::ArtifactChanged { path: path.to_owned() });
    }

    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; ARTIFACT_DIGEST_BUFFER_BYTES];
    let mut offset = 0_u64;
    while offset < expected.length {
        let remaining = expected.length - offset;
        let requested = usize::try_from(remaining).unwrap_or(usize::MAX).min(buffer.len());
        let read = loop {
            // SAFETY: file and buffer remain live; requested is bounded by the
            // buffer; offset is below the 2-TiB artifact ceiling and fits off_t.
            let result = unsafe {
                libc::pread(
                    file.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    requested,
                    offset as libc::off_t,
                )
            };
            if result == -1 {
                let source = io::Error::last_os_error();
                if source.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(ArtifactError::Io {
                    operation: "hash sealed artifact",
                    path: path.to_owned(),
                    source,
                });
            }
            break result as usize;
        };
        if read == 0 || read > requested {
            return Err(ArtifactError::ArtifactChanged { path: path.to_owned() });
        }
        hasher.update(&buffer[..read]);
        offset = offset.checked_add(read as u64).ok_or(ArtifactError::ArtifactTooLarge {
            path: path.to_owned(),
            maximum: expected.length,
            found: u64::MAX,
        })?;
    }

    let trailing = loop {
        // SAFETY: the one-byte buffer and descriptor remain live and the
        // offset is bounded as above.
        let result = unsafe { libc::pread(file.as_raw_fd(), buffer.as_mut_ptr().cast(), 1, offset as libc::off_t) };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(ArtifactError::Io {
                operation: "confirm sealed artifact end",
                path: path.to_owned(),
                source,
            });
        }
        break result;
    };
    if trailing != 0 {
        return Err(ArtifactError::ArtifactChanged { path: path.to_owned() });
    }
    let after = file.metadata().map_err(|source| ArtifactError::Io {
        operation: "inspect sealed artifact after hashing",
        path: path.to_owned(),
        source,
    })?;
    if FileWitness::from_metadata(&after) != expected {
        return Err(ArtifactError::ArtifactChanged { path: path.to_owned() });
    }
    Ok(hasher.finalize().into())
}

// Linux has no unlinkat variant that accepts an expected inode. Emission runs
// beneath the derivation execution lock after all build/analyzer processes are
// gone, and the root is a freshly recreated single-mutator directory while the
// staging child is mode 0700. Within that trust boundary this identity check
// prevents stale or foreign names from being removed. Renaming to a quarantine
// first cannot strengthen the guarantee: a racer can replace the source after
// authentication, causing renameat2 to move the foreign inode, and a later
// no-replace restoration can itself collide. Never broaden this helper to a
// directory writable by concurrent same-UID actors without kernel support for
// conditional unlink.
fn remove_owned_entry(
    directory: &DirectoryHandle,
    name: &[u8],
    identity: Identity,
    directory_entry: bool,
    missing_ok: bool,
    operation: &'static str,
) -> Result<(), ArtifactError> {
    let path = directory.display(name);
    let Some((metadata, current_identity)) = directory.inspect(name, operation)? else {
        return if missing_ok {
            Ok(())
        } else {
            Err(ArtifactError::OwnershipChanged { path })
        };
    };
    if current_identity != identity || metadata.file_type().is_dir() != directory_entry {
        return Err(ArtifactError::OwnershipChanged { path });
    }
    let name = c_name(name, &path)?;
    let flags = if directory_entry { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: the parent descriptor and single-component NUL-terminated name
    // remain live. unlinkat never follows a final symlink.
    if unsafe { libc::unlinkat(directory.file.as_raw_fd(), name.as_ptr(), flags) } == -1 {
        return Err(ArtifactError::Io {
            operation,
            path,
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

fn rename_noreplace_at(
    source_parent: &DirectoryHandle,
    source_name: &[u8],
    target_parent: &DirectoryHandle,
    target_name: &[u8],
) -> Result<(), ArtifactError> {
    let source_path = source_parent.display(source_name);
    let target_path = target_parent.display(target_name);
    let source_name = c_name(source_name, &source_path)?;
    let target_name = c_name(target_name, &target_path)?;
    // SAFETY: both descriptors and names remain live. Linux renameat2 with
    // RENAME_NOREPLACE atomically installs the staged inode or changes nothing.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_parent.file.as_raw_fd(),
            source_name.as_ptr(),
            target_parent.file.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == -1 {
        Err(ArtifactError::Io {
            operation: "atomically publish staged artifact without replacement",
            path: target_path,
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn validate_artifact_name(name: &str) -> Result<(), ArtifactError> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 255
        || matches!(bytes, b"." | b"..")
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(ArtifactError::InvalidName { name: name.to_owned() });
    }
    Ok(())
}

fn c_name(name: &[u8], path: &Path) -> Result<CString, ArtifactError> {
    if name.contains(&0) {
        return Err(ArtifactError::InvalidName {
            name: path.display().to_string(),
        });
    }
    let requested = name.len().checked_add(1).ok_or(ArtifactError::ResourceLimit {
        resource: "artifact C string bytes",
        limit: usize::MAX,
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(requested)
        .map_err(|source| ArtifactError::Allocation {
            resource: "artifact C string bytes",
            requested,
            detail: source.to_string(),
        })?;
    bytes.extend_from_slice(name);
    bytes.push(0);
    CString::from_vec_with_nul(bytes).map_err(|_| ArtifactError::InvalidName {
        name: path.display().to_string(),
    })
}

fn copy_bytes(bytes: &[u8], resource: &'static str) -> Result<Vec<u8>, ArtifactError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|source| ArtifactError::Allocation {
            resource,
            requested: bytes.len(),
            detail: source.to_string(),
        })?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn one_name(name: &[u8]) -> Result<Vec<Vec<u8>>, ArtifactError> {
    let mut names = Vec::new();
    names.try_reserve_exact(1).map_err(|source| ArtifactError::Allocation {
        resource: "single artifact inventory entry",
        requested: 1,
        detail: source.to_string(),
    })?;
    names.push(copy_bytes(name, "single artifact inventory name")?);
    Ok(names)
}

fn copy_name_list(names: &[Vec<u8>], resource: &'static str) -> Result<Vec<Vec<u8>>, ArtifactError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(names.len())
        .map_err(|source| ArtifactError::Allocation {
            resource,
            requested: names.len(),
            detail: source.to_string(),
        })?;
    for name in names {
        copy.push(copy_bytes(name, resource)?);
    }
    Ok(copy)
}

fn descendant_resolution() -> u64 {
    libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV
}

fn openat2_file(
    dirfd: RawFd,
    path: &[u8],
    flags: i32,
    mode: u32,
    resolve: u64,
    display_path: &Path,
) -> io::Result<File> {
    let path = cstring_io(path)?;
    // SAFETY: zero is valid for every open_how field before the public fields
    // used by this kernel ABI are initialized below.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: path is NUL-terminated, how is initialized, and a successful
    // syscall returns a fresh descriptor owned by this process.
    let result = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat2 returned a fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(result as RawFd) };
    Ok(File::from_parts(descriptor.into(), display_path))
}

fn cstring_io(bytes: &[u8]) -> io::Result<CString> {
    if bytes.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"));
    }
    let requested = bytes
        .len()
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::OutOfMemory, "path byte count overflow"))?;
    let mut terminated = Vec::new();
    terminated
        .try_reserve_exact(requested)
        .map_err(|source| io::Error::new(io::ErrorKind::OutOfMemory, source))?;
    terminated.extend_from_slice(bytes);
    terminated.push(0);
    CString::from_vec_with_nul(terminated)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains an interior NUL"))
}

#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("{operation} at {path:?}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{role} at {path:?} is not the expected {expected}")]
    UnexpectedKind {
        role: &'static str,
        path: PathBuf,
        expected: &'static str,
    },
    #[error("artifact name {name:?} is not one safe path component")]
    InvalidName { name: String },
    #[error("duplicate emitted artifact name {name:?}")]
    DuplicateName { name: String },
    #[error("artifact writer requested undeclared name {name:?}")]
    UnexpectedName { name: String },
    #[error("artifact {name:?} was prepared more than once")]
    AlreadyPrepared { name: String },
    #[error("artifact {name:?} was never prepared")]
    NotPrepared { name: String },
    #[error("artifact content scratch is unavailable")]
    ScratchUnavailable,
    #[error("owned artifact path changed identity or type: {path:?}")]
    OwnershipChanged { path: PathBuf },
    #[error("sealed artifact metadata changed: {path:?}")]
    ArtifactChanged { path: PathBuf },
    #[error("sealed artifact content digest changed: {path:?}")]
    DigestChanged { path: PathBuf },
    #[error("artifact directory changed during exact enumeration: {path:?}")]
    DirectoryChanged { path: PathBuf },
    #[error("{role} {path:?} has the wrong entries (expected {expected:?}, found {found:?})")]
    InventoryMismatch {
        role: &'static str,
        path: PathBuf,
        expected: Vec<Vec<u8>>,
        found: Vec<Vec<u8>>,
    },
    #[error("{role} {path:?} has mode {found:#06o}; expected {expected:#06o}")]
    ModeMismatch {
        role: &'static str,
        path: PathBuf,
        expected: u32,
        found: u32,
    },
    #[error("artifact {path:?} is {found} bytes; maximum is {maximum}")]
    ArtifactTooLarge { path: PathBuf, maximum: u64, found: u64 },
    #[error("{resource} exceeds finite limit {limit}")]
    ResourceLimit { resource: &'static str, limit: usize },
    #[error("failed to reserve {requested} units for {resource}: {detail}")]
    Allocation {
        resource: &'static str,
        requested: usize,
        detail: String,
    },
    #[error("artifact rollback was incomplete: {failures:?}")]
    Cleanup { failures: Vec<String> },
    #[error("cannot safely remove an artifact path whose created inode could not be authenticated: {path:?}")]
    UnprovenCleanup { path: PathBuf },
    #[error("{primary}; rollback also failed: {cleanup}")]
    Rollback {
        primary: Box<ArtifactError>,
        cleanup: Box<ArtifactError>,
    },
}

#[derive(Debug)]
pub struct Package<'a> {
    pub name: &'a str,
    pub build_release: NonZeroU64,
    pub architecture: Architecture,
    pub identity: &'a PackageIdentity,
    pub definition: &'a ResolvedOutput,
    pub analysis: analysis::Bucket,
    provides_exclude: Vec<Regex>,
    runtime_exclude: Vec<Regex>,
    jobs: u32,
}

impl<'a> Package<'a> {
    pub fn new_with_architecture(
        name: &'a str,
        identity: &'a PackageIdentity,
        definition: &'a ResolvedOutput,
        analysis: analysis::Bucket,
        build_release: NonZeroU64,
        architecture: Architecture,
        jobs: u32,
    ) -> Self {
        let provides_exclude = compile_exclusions(&definition.provides_exclude);
        let runtime_exclude = compile_exclusions(&definition.runtime_exclude);
        Self {
            name,
            architecture,
            identity,
            definition,
            analysis,
            provides_exclude,
            runtime_exclude,
            build_release,
            jobs,
        }
    }

    pub fn filename(&self) -> String {
        super::stone_artefact_filename(
            self.name,
            &self.identity.version,
            self.identity.source_release,
            self.build_release.get(),
            self.architecture,
        )
    }

    pub fn dependencies(&self) -> Vec<Dependency> {
        self.analysis
            .dependencies()
            .cloned()
            .chain(self.definition.runtime_inputs.iter().cloned())
            .filter(|dependency| {
                self.runtime_exclude
                    .iter()
                    .all(|filter| !filter.is_match(&dependency.to_string()))
            })
            .collect()
    }

    pub fn providers(&self) -> Vec<Provider> {
        self.analysis
            .providers()
            .filter(|provider| {
                self.provides_exclude
                    .iter()
                    .all(|filter| !filter.is_match(&provider.to_string()))
            })
            .cloned()
            .collect()
    }

    pub fn meta(&self) -> Meta {
        Meta {
            name: self.name.to_owned().into(),
            version_identifier: self.identity.version.clone(),
            source_release: self.identity.source_release,
            build_release: self.build_release.get(),
            architecture: self.architecture.to_string(),
            summary: self.definition.summary.clone().unwrap_or_default(),
            description: self.definition.description.clone().unwrap_or_default(),
            source_id: self.identity.name.clone(),
            homepage: self.identity.homepage.clone(),
            licenses: self.identity.licenses.clone().into_iter().sorted().collect(),
            dependencies: self.dependencies().into_iter().collect(),
            providers: self.providers().into_iter().collect(),
            conflicts: self.definition.conflicts.iter().cloned().collect(),
            uri: None,
            hash: None,
            download_size: None,
        }
    }

    fn meta_payload(&self, recipe_fingerprint: &str, derivation_id: &DerivationId) -> Vec<StonePayloadMetaRecord> {
        Self::with_derivation_provenance(self.meta().to_stone_payload(), recipe_fingerprint, derivation_id)
    }

    fn with_derivation_provenance(
        mut payload: Vec<StonePayloadMetaRecord>,
        recipe_fingerprint: &str,
        derivation_id: &DerivationId,
    ) -> Vec<StonePayloadMetaRecord> {
        // SourceRef is an existing, optional stone metadata extension point. The
        // namespaced value is ignored by older package readers but retained in
        // package and build-manifest payloads for provenance-aware tooling.
        payload.push(StonePayloadMetaRecord {
            tag: StonePayloadMetaTag::SourceRef,
            primitive: StonePayloadMetaPrimitive::String(format!(
                "{RECIPE_FINGERPRINT_SOURCE_REF_PREFIX}{recipe_fingerprint}"
            )),
        });
        payload.push(StonePayloadMetaRecord {
            tag: StonePayloadMetaTag::SourceRef,
            primitive: StonePayloadMetaPrimitive::String(format!("{DERIVATION_ID_SOURCE_REF_PREFIX}{derivation_id}")),
        });
        payload
    }
}

fn compile_exclusions(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .map(|pattern| Regex::new(pattern).expect("output exclusions were validated before package emission"))
        .collect()
}

impl PartialEq for Package<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.name.eq(other.name)
    }
}

impl Eq for Package<'_> {}

impl PartialOrd for Package<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Package<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(other.name)
    }
}

pub fn emit_frozen(
    paths: &Paths,
    identity: &PackageIdentity,
    recipe_fingerprint: &str,
    build_deps: impl IntoIterator<Item = Dependency>,
    architecture: Architecture,
    packages: &[Package<'_>],
    derivation_id: &DerivationId,
    sealed: &collect::SealedTree,
) -> Result<(), Error> {
    verify_unique_layout_targets(packages)?;
    sealed.verify().map_err(|source| Error::VerifiedInventory { source })?;
    let mut manifest = Manifest::new(identity, recipe_fingerprint, build_deps, derivation_id);
    for package in packages {
        if package.definition.include_in_manifest {
            manifest.add_package(package);
        }
    }

    let binary_manifest_name = super::binary_manifest_filename(architecture);
    let json_manifest_name = super::jsonc_manifest_filename(architecture);
    if packages.len() > MAX_EMITTED_ARTIFACTS.saturating_sub(2) {
        return Err(Error::Artifact {
            source: ArtifactError::ResourceLimit {
                resource: "emitted package artifacts",
                limit: MAX_EMITTED_ARTIFACTS.saturating_sub(2),
            },
        });
    }
    let mut specs = Vec::new();
    specs
        .try_reserve(packages.len().saturating_add(2))
        .map_err(|source| Error::Allocation {
            resource: "expected artifact names",
            requested: packages.len().saturating_add(2),
            detail: source.to_string(),
        })?;
    specs.extend(packages.iter().map(|package| ArtifactSpec::stone(package.filename())));
    specs.push(ArtifactSpec::manifest(binary_manifest_name.clone()));
    specs.push(ArtifactSpec::manifest(json_manifest_name.clone()));
    let mut sink = ArtifactSink::new(&paths.artefacts().guest, specs).context(ArtifactSnafu)?;

    println!("Packaging");

    let emission = (|| {
        for package in packages {
            emit_package(&mut sink, package, recipe_fingerprint, derivation_id)?;
        }

        manifest
            .write_binary(sink.writer(&binary_manifest_name).context(ArtifactSnafu)?)
            .context(ManifestSnafu)?;
        manifest
            .write_json(sink.writer(&json_manifest_name).context(ArtifactSnafu)?)
            .context(ManifestSnafu)?;
        for package in packages {
            verify_paths(&package.analysis.paths)?;
        }
        sealed.verify().map_err(|source| Error::VerifiedInventory { source })?;
        Ok(())
    })();
    if let Err(primary) = emission {
        return match sink.abort() {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(Error::ArtifactRollback {
                primary: Box::new(primary),
                cleanup,
            }),
        };
    }
    sink.commit().context(ArtifactSnafu)?;

    println!();

    Ok(())
}

fn verify_unique_layout_targets(packages: &[Package<'_>]) -> Result<(), Error> {
    let total = packages.iter().try_fold(0usize, |total, package| {
        total
            .checked_add(package.analysis.paths.len())
            .ok_or(Error::SizeOverflow {
                resource: "package layout targets",
            })
    })?;
    let mut targets = Vec::new();
    try_reserve(&mut targets, total, "package layout targets")?;
    for package in packages {
        for info in &package.analysis.paths {
            info.check_deadline().map_err(|source| Error::VerifiedInput {
                path: info.path.clone(),
                source,
            })?;
            let target = info.layout.file.target().trim_start_matches('/');
            if is_reserved_usr_layout_target(target) {
                return Err(Error::ReservedLayoutTarget {
                    target: format!("/usr/{target}"),
                    package: package.name.to_owned(),
                    path: info.path.clone(),
                });
            }
            targets.push((
                target,
                package.name,
                &info.path,
                matches!(info.layout.file, stone::StonePayloadLayoutFile::Directory(_)),
            ));
        }
    }
    targets.sort_unstable_by(|left, right| {
        left.0
            .cmp(right.0)
            .then_with(|| left.1.cmp(right.1))
            .then_with(|| left.2.cmp(right.2))
    });
    for pair in targets.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(Error::DuplicateLayoutTarget {
                target: format!("/{}", pair[0].0),
                first_package: pair[0].1.to_owned(),
                first_path: pair[0].2.to_owned(),
                second_package: pair[1].1.to_owned(),
                second_path: pair[1].2.to_owned(),
            });
        }
    }

    // Exact duplicates are rejected above, including duplicate directories.
    // A directory may be the ancestor of another layout target, but every
    // other inode kind would require the installer to materialize the same
    // target as both a terminal and a directory. Normalized `/usr` aliases can
    // otherwise make this conflict arise from distinct source paths.
    for descendant in &targets {
        let mut ancestor = descendant.0;
        while let Some(separator) = ancestor.rfind('/') {
            ancestor = &ancestor[..separator];
            if ancestor.is_empty() {
                break;
            }
            if let Ok(index) = targets.binary_search_by(|candidate| candidate.0.cmp(ancestor)) {
                let candidate = &targets[index];
                if !candidate.3 {
                    return Err(Error::AncestorLayoutTarget {
                        ancestor: format!("/{}", candidate.0),
                        ancestor_package: candidate.1.to_owned(),
                        ancestor_path: candidate.2.to_owned(),
                        descendant: format!("/{}", descendant.0),
                        descendant_package: descendant.1.to_owned(),
                        descendant_path: descendant.2.to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn emit_package(
    sink: &mut ArtifactSink,
    package: &Package<'_>,
    recipe_fingerprint: &str,
    derivation_id: &DerivationId,
) -> Result<(), Error> {
    let filename = package.filename();

    verify_paths(&package.analysis.paths)?;

    // Choose a deterministic representative for each content hash, then emit
    // larger blobs first. Collection identities, rather than host paths, are
    // retained for every selected file.
    let mut hashed_files = Vec::new();
    try_reserve(
        &mut hashed_files,
        package.analysis.paths.len(),
        "package content references",
    )?;
    for info in &package.analysis.paths {
        if let Some(hash) = info.file_hash() {
            hashed_files.push((hash, info));
        }
    }
    hashed_files.sort_unstable_by(|(left_hash, left), (right_hash, right)| {
        left_hash
            .cmp(right_hash)
            .then_with(|| left.target_path.cmp(&right.target_path))
            .then_with(|| left.path.cmp(&right.path))
    });
    hashed_files.dedup_by(|left, right| left.0 == right.0);
    hashed_files.sort_unstable_by(|(left_hash, left), (right_hash, right)| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left_hash.cmp(right_hash))
            .then_with(|| left.target_path.cmp(&right.target_path))
    });

    let mut total_file_size = 0u64;
    for (_, info) in &hashed_files {
        total_file_size = total_file_size.checked_add(info.size).ok_or(Error::SizeOverflow {
            resource: "package content bytes",
        })?;
    }

    let pb = ProgressBar::new(total_file_size)
        .with_message(format!("Generating {filename}"))
        .with_style(
            ProgressStyle::with_template(" {spinner} |{percent:>3}%| {wide_msg} {binary_bytes_per_sec:>.dim} ")
                .unwrap()
                .tick_chars("--=≡■≡=--"),
        );
    pb.enable_steady_tick(Duration::from_millis(150));

    let (out_file, temp_content) = sink.package_writers(&filename).context(ArtifactSnafu)?;

    // Create stone binary writer
    let mut writer = StoneWriter::new(&mut *out_file, StoneHeaderV1FileType::Binary).context(StoneBinaryWriterSnafu)?;

    // Add metadata
    {
        writer
            .add_payload(package.meta_payload(recipe_fingerprint, derivation_id).as_slice())
            .context(StoneBinaryWriterSnafu)?;
    }

    // Add layouts
    {
        let mut layouts = Vec::new();
        try_reserve(&mut layouts, package.analysis.paths.len(), "package layout records")?;
        layouts.extend(package.analysis.paths.iter().map(|path| path.layout.clone()));
        layouts.sort_unstable_by(|left, right| left.file.target().cmp(right.file.target()));
        if !layouts.is_empty() {
            writer.add_payload(layouts.as_slice()).context(StoneBinaryWriterSnafu)?;
        }
    }

    // Only add content payload if we have some files
    if !hashed_files.is_empty() {
        // Convert to content writer using pledged size = total size of all files
        let mut writer = writer
            .with_content(temp_content, Some(total_file_size), package.jobs)
            .context(StoneBinaryWriterSnafu)?;

        for (_, info) in hashed_files {
            let mut file = info.open_verified().map_err(|source| Error::VerifiedInput {
                path: info.path.clone(),
                source,
            })?;
            let write_result = {
                let mut progress = pb.wrap_read(&mut file);
                writer.add_content(&mut progress)
            };
            let verify_result = file.finish();
            finish_content_write(&info.path, write_result, verify_result)?;
        }

        // Finalize & flush
        writer.finalize().context(StoneBinaryWriterSnafu)?;
        out_file.flush().context(IoSnafu)?;
        verify_paths(&package.analysis.paths)?;
    } else {
        // Finalize & flush
        writer.finalize().context(StoneBinaryWriterSnafu)?;
        out_file.flush().context(IoSnafu)?;
        verify_paths(&package.analysis.paths)?;
    }

    pb.suspend(|| println!("{} {filename}", "Prepared".green()));
    pb.finish_and_clear();

    Ok(())
}

fn finish_content_write(
    path: &Path,
    write_result: Result<(), StoneWriteError>,
    verify_result: Result<(), collect::Error>,
) -> Result<(), Error> {
    // Always finish descriptor verification, but do not replace a primary
    // Stone writer failure with the expected short-read consequence.
    write_result.context(StoneBinaryWriterSnafu)?;
    verify_result.map_err(|source| Error::VerifiedInput {
        path: path.to_owned(),
        source,
    })
}

fn verify_paths(paths: &[collect::PathInfo]) -> Result<(), Error> {
    for info in paths {
        info.verify_unchanged().map_err(|source| Error::VerifiedInput {
            path: info.path.clone(),
            source,
        })?;
    }
    Ok(())
}

fn try_reserve<T>(items: &mut Vec<T>, additional: usize, resource: &'static str) -> Result<(), Error> {
    items.try_reserve(additional).map_err(|source| Error::Allocation {
        resource,
        requested: additional,
        detail: source.to_string(),
    })
}

#[cfg(test)]
mod verification_tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn test_sink(root: &Path, names: &[(&str, u64)]) -> ArtifactSink {
        ArtifactSink::new(
            root,
            names
                .iter()
                .map(|(name, max_bytes)| ArtifactSpec {
                    name: (*name).to_owned(),
                    max_bytes: *max_bytes,
                })
                .collect(),
        )
        .unwrap()
    }

    fn direct_names(root: &Path) -> Vec<String> {
        let mut names = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn artifact_sink_publishes_only_the_exact_read_only_set() {
        let root = tempfile::tempdir().unwrap();
        let mut sink = test_sink(root.path(), &[("a.stone", 32), ("manifest.bin", 32)]);
        sink.writer("a.stone").unwrap().write_all(b"stone").unwrap();
        sink.writer("manifest.bin").unwrap().write_all(b"manifest").unwrap();

        sink.commit().unwrap();

        assert_eq!(direct_names(root.path()), ["a.stone", "manifest.bin"]);
        assert_eq!(std::fs::read(root.path().join("a.stone")).unwrap(), b"stone");
        assert_eq!(std::fs::read(root.path().join("manifest.bin")).unwrap(), b"manifest");
        for name in ["a.stone", "manifest.bin"] {
            let metadata = std::fs::symlink_metadata(root.path().join(name)).unwrap();
            assert!(metadata.file_type().is_file());
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(metadata.permissions().mode() & 0o7777, 0o444);
        }
    }

    #[test]
    fn real_contentful_stone_emission_survives_transactional_staging() {
        let input = tempfile::tempdir().unwrap();
        let largest = input.path().join("usr/bin/largest");
        let equal_a = input.path().join("usr/bin/equal-a");
        let equal_b = input.path().join("usr/bin/equal-b");
        std::fs::create_dir_all(largest.parent().unwrap()).unwrap();
        std::fs::write(&largest, b"the largest contentful stone payload").unwrap();
        std::fs::write(&equal_a, b"aaaa").unwrap();
        std::fs::write(&equal_b, b"bbbb").unwrap();
        let mut collector = collect::Collector::new(input.path());
        collector
            .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
            .unwrap();
        let mut hasher = stone::StoneDigestWriterHasher::new();
        // Deliberately feed both layout targets and content sizes in a
        // non-canonical order. The emitter, not traversal accident, owns the
        // Stone wire order.
        let infos = [&equal_b, &largest, &equal_a]
            .into_iter()
            .map(|source| collector.path(source, &mut hasher).unwrap())
            .collect::<Vec<_>>();
        let mut bucket = analysis::Bucket::default();
        bucket.paths.extend(infos);
        let plan = test_derivation_plan();
        let definition = ResolvedOutput::default();
        let package = Package::new_with_architecture(
            "example",
            &plan.package,
            &definition,
            bucket,
            NonZeroU64::new(1).unwrap(),
            Architecture::X86_64,
            1,
        );
        let filename = package.filename();
        let output = tempfile::tempdir().unwrap();
        let mut sink = test_sink(output.path(), &[(filename.as_str(), MAX_STONE_ARTIFACT_BYTES)]);

        emit_package(
            &mut sink,
            &package,
            &plan.provenance.recipe.sha256,
            &plan.derivation_id(),
        )
        .unwrap();
        sink.commit().unwrap();

        let mut stone = File::open(output.path().join(filename)).unwrap();
        let payloads = forge::util::stone_payloads(&mut stone).unwrap();
        assert!(payloads.iter().any(|payload| payload.meta().is_some()));
        let layouts = payloads.iter().find_map(|payload| payload.layout()).unwrap();
        assert_eq!(
            layouts
                .body
                .iter()
                .map(|record| record.file.target())
                .collect::<Vec<_>>(),
            ["bin/equal-a", "bin/equal-b", "bin/largest"]
        );
        let indices = payloads.iter().find_map(|payload| payload.index()).unwrap();
        assert!(indices.body.windows(2).all(|pair| {
            let left_size = pair[0].end - pair[0].start;
            let right_size = pair[1].end - pair[1].start;
            left_size > right_size || (left_size == right_size && pair[0].digest < pair[1].digest)
        }));
        assert_eq!(indices.body[0].end - indices.body[0].start, 36);
        assert_eq!(indices.body[1].end - indices.body[1].start, 4);
        assert_eq!(indices.body[2].end - indices.body[2].start, 4);
        assert!(payloads.iter().any(|payload| payload.content().is_some()));
    }

    #[test]
    fn bounded_artifact_failure_removes_every_owned_name() {
        let root = tempfile::tempdir().unwrap();
        let mut sink = test_sink(root.path(), &[("bounded.stone", 4)]);

        let error = sink.writer("bounded.stone").unwrap().write_all(b"12345").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        sink.abort().unwrap();

        assert!(direct_names(root.path()).is_empty());
    }

    #[test]
    fn bounded_artifact_seek_accepts_exact_limit_and_rejects_limit_plus_one() {
        let root = tempfile::tempdir().unwrap();
        let mut sink = test_sink(root.path(), &[("bounded.stone", 4)]);
        let writer = sink.writer("bounded.stone").unwrap();

        writer.write_all(b"1234").unwrap();
        assert_eq!(writer.seek(SeekFrom::Start(4)).unwrap(), 4);
        assert_eq!(writer.file.metadata().unwrap().len(), 4);
        assert_eq!(
            writer.seek(SeekFrom::Start(5)).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        assert_eq!(
            writer.seek(SeekFrom::Current(1)).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        assert_eq!(
            writer.seek(SeekFrom::End(1)).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        assert_eq!(writer.write(b"5").unwrap_err().kind(), io::ErrorKind::FileTooLarge);
        assert_eq!(writer.file.metadata().unwrap().len(), 4);

        sink.abort().unwrap();
        assert!(direct_names(root.path()).is_empty());
    }

    #[test]
    fn publication_collision_after_one_rename_rolls_back_owned_final() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_owned();
        let mut sink = test_sink(root.path(), &[("a.stone", 32), ("b.stone", 32)]);
        sink.writer("a.stone").unwrap().write_all(b"owned-a").unwrap();
        sink.writer("b.stone").unwrap().write_all(b"owned-b").unwrap();

        let result = sink.commit_with_hook(|index, _| {
            if index == 0 {
                std::fs::write(root_path.join("b.stone"), b"foreign-blocker").unwrap();
            }
        });

        assert!(result.is_err());
        assert_eq!(direct_names(root.path()), ["b.stone"]);
        assert_eq!(std::fs::read(root.path().join("b.stone")).unwrap(), b"foreign-blocker");
    }

    #[test]
    fn staged_same_size_mutation_immediately_before_rename_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut sink = test_sink(root.path(), &[("a.stone", 32)]);
        sink.writer("a.stone").unwrap().write_all(b"owned-bytes").unwrap();

        let result = sink.commit_with_hooks(
            |_, path| {
                let before = std::fs::metadata(path).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
                let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
                file.write_all(b"other-bytes").unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
                file.set_times(
                    std::fs::FileTimes::new()
                        .set_accessed(before.accessed().unwrap())
                        .set_modified(before.modified().unwrap()),
                )
                .unwrap();
            },
            |_, _| {},
        );

        assert!(matches!(result, Err(ArtifactError::DigestChanged { .. })));
        assert!(direct_names(root.path()).is_empty());
    }

    #[test]
    fn final_inode_swap_is_detected_without_deleting_the_replacement() {
        let root = tempfile::tempdir().unwrap();
        let mut sink = test_sink(root.path(), &[("a.stone", 32), ("b.stone", 32)]);
        sink.writer("a.stone").unwrap().write_all(b"owned-a").unwrap();
        sink.writer("b.stone").unwrap().write_all(b"owned-b").unwrap();

        let result = sink.commit_with_hook(|index, path| {
            if index == 0 {
                std::fs::remove_file(path).unwrap();
                std::fs::write(path, b"foreign-replacement").unwrap();
            }
        });

        assert!(matches!(result, Err(ArtifactError::Rollback { .. })));
        assert_eq!(direct_names(root.path()), ["a.stone"]);
        assert_eq!(
            std::fs::read(root.path().join("a.stone")).unwrap(),
            b"foreign-replacement"
        );
    }

    #[test]
    fn same_inode_truncation_after_publication_is_detected_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let mut sink = test_sink(root.path(), &[("a.stone", 32)]);
        sink.writer("a.stone").unwrap().write_all(b"owned-bytes").unwrap();

        let result = sink.commit_with_hook(|_, path| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(path)
                .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
        });

        assert!(matches!(result, Err(ArtifactError::ArtifactChanged { .. })));
        assert!(direct_names(root.path()).is_empty());
    }

    #[test]
    fn same_inode_same_size_overwrite_after_publication_is_detected_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let mut sink = test_sink(root.path(), &[("a.stone", 32)]);
        sink.writer("a.stone").unwrap().write_all(b"owned-bytes").unwrap();

        let result = sink.commit_with_hook(|_, path| {
            let metadata_before = std::fs::symlink_metadata(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::write(path, b"other-bytes").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
            let metadata_after = std::fs::symlink_metadata(path).unwrap();
            assert_eq!(metadata_before.ino(), metadata_after.ino());
            assert_eq!(metadata_before.len(), metadata_after.len());
        });

        assert!(matches!(result, Err(ArtifactError::ArtifactChanged { .. })));
        assert!(direct_names(root.path()).is_empty());
    }

    #[test]
    fn replaced_public_root_is_rejected_and_only_the_pinned_root_is_cleaned() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("artifacts");
        let moved = parent.path().join("moved-artifacts");
        std::fs::create_dir(&root).unwrap();
        let mut sink = test_sink(&root, &[("a.stone", 32)]);
        sink.writer("a.stone").unwrap().write_all(b"owned").unwrap();

        std::fs::rename(&root, &moved).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("sentinel"), b"do not delete").unwrap();

        assert!(sink.commit().is_err());
        assert!(direct_names(&moved).is_empty());
        assert_eq!(direct_names(&root), ["sentinel"]);
        assert_eq!(std::fs::read(root.join("sentinel")).unwrap(), b"do not delete");
    }

    #[test]
    fn preexisting_artifact_root_entries_are_never_reused_or_removed() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.stone"), b"preexisting").unwrap();

        assert!(ArtifactSink::new(root.path(), vec![ArtifactSpec::stone("a.stone".to_owned())]).is_err());
        assert_eq!(direct_names(root.path()), ["a.stone"]);
        assert_eq!(std::fs::read(root.path().join("a.stone")).unwrap(), b"preexisting");
    }

    #[test]
    fn emitter_rejects_a_path_replaced_after_collection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"payload").unwrap();
        let mut collector = collect::Collector::new(root.path());
        collector
            .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
            .unwrap();
        let info = collector
            .path(&path, &mut stone::StoneDigestWriterHasher::new())
            .unwrap();
        std::fs::rename(&path, root.path().join("old")).unwrap();
        std::fs::write(&path, b"payload").unwrap();

        assert!(matches!(verify_paths(&[info]), Err(Error::VerifiedInput { .. })));
    }

    #[test]
    fn duplicate_normalized_layout_targets_are_rejected_before_emission() {
        let root = tempfile::tempdir().unwrap();
        let usr = root.path().join("usr/bin/tool");
        let root_bin = root.path().join("bin/tool");
        std::fs::create_dir_all(usr.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root_bin.parent().unwrap()).unwrap();
        std::fs::write(&usr, b"usr").unwrap();
        std::fs::write(&root_bin, b"root").unwrap();
        let mut collector = collect::Collector::new(root.path());
        collector
            .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
            .unwrap();
        let mut hasher = stone::StoneDigestWriterHasher::new();
        let usr = collector.path(&usr, &mut hasher).unwrap();
        let root_bin = collector.path(&root_bin, &mut hasher).unwrap();
        let plan = test_derivation_plan();
        let definition = ResolvedOutput::default();
        let package = |name, path| {
            let mut bucket = analysis::Bucket::default();
            bucket.paths.push(path);
            Package::new_with_architecture(
                name,
                &plan.package,
                &definition,
                bucket,
                NonZeroU64::new(1).unwrap(),
                Architecture::X86_64,
                1,
            )
        };
        let packages = [package("first", usr), package("second", root_bin)];
        assert!(matches!(
            verify_unique_layout_targets(&packages),
            Err(Error::DuplicateLayoutTarget { .. })
        ));
    }

    #[test]
    fn reserved_system_metadata_target_is_rejected_before_artifact_sink_creation() {
        let input = tempfile::tempdir().unwrap();
        let reserved_path = input.path().join("usr/.cast-tree-id/forged-child");
        std::fs::create_dir_all(reserved_path.parent().unwrap()).unwrap();
        std::fs::write(&reserved_path, b"forged marker").unwrap();

        let mut collector = collect::Collector::new(input.path());
        collector
            .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
            .unwrap();
        let reserved = collector
            .path(&reserved_path, &mut stone::StoneDigestWriterHasher::new())
            .unwrap();
        let sealed = collector.seal().unwrap();

        let plan = test_derivation_plan();
        let definition = ResolvedOutput::default();
        let mut bucket = analysis::Bucket::default();
        bucket.paths.push(reserved);
        let package = Package::new_with_architecture(
            "reserved-owner",
            &plan.package,
            &definition,
            bucket,
            NonZeroU64::new(1).unwrap(),
            Architecture::X86_64,
            1,
        );

        let runtime = crate::private_tempdir();
        let output = tempfile::tempdir().unwrap();
        let artifact_root = tempfile::tempdir().unwrap();
        let recipe =
            crate::Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu"))
                .unwrap();
        let mut layout = plan.layout.clone();
        layout.artifacts_dir = artifact_root.path().to_string_lossy().into_owned();
        let paths = Paths::new(&recipe, layout, runtime.path(), output.path()).unwrap();

        assert!(matches!(
            emit_frozen(
                &paths,
                &plan.package,
                &plan.provenance.recipe.sha256,
                std::iter::empty(),
                Architecture::X86_64,
                &[package],
                &plan.derivation_id(),
                &sealed,
            ),
            Err(Error::ReservedLayoutTarget {
                target,
                package,
                path,
            }) if target == "/usr/.cast-tree-id/forged-child"
                && package == "reserved-owner"
                && path == reserved_path
        ));
        assert!(direct_names(artifact_root.path()).is_empty());
    }

    #[test]
    fn near_system_metadata_names_remain_legal_for_mason_layouts() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let definition = ResolvedOutput::default();
        let mut collector = collect::Collector::new(root.path());
        collector
            .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
            .unwrap();
        let mut hasher = stone::StoneDigestWriterHasher::new();
        let mut bucket = analysis::Bucket::default();

        let near_names = ["usr/.cast-tree-id-old", "usr/.stateID.old/child"];
        for relative in near_names {
            let path = root.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"ordinary package data").unwrap();
        }
        for relative in near_names {
            let path = root.path().join(relative);
            bucket.paths.push(collector.path(&path, &mut hasher).unwrap());
        }

        let package = Package::new_with_architecture(
            "near-names",
            &plan.package,
            &definition,
            bucket,
            NonZeroU64::new(1).unwrap(),
            Architecture::X86_64,
            1,
        );
        verify_unique_layout_targets(&[package]).unwrap();
    }

    #[test]
    fn non_directory_normalized_ancestor_is_rejected_before_emission() {
        let root = tempfile::tempdir().unwrap();
        let normalized_ancestor = root.path().join("usr/bin");
        let descendant = root.path().join("bin/tool");
        std::fs::create_dir_all(normalized_ancestor.parent().unwrap()).unwrap();
        std::fs::create_dir_all(descendant.parent().unwrap()).unwrap();
        std::fs::write(&normalized_ancestor, b"not a directory").unwrap();
        std::fs::write(&descendant, b"payload").unwrap();
        let mut collector = collect::Collector::new(root.path());
        collector
            .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
            .unwrap();
        let mut hasher = stone::StoneDigestWriterHasher::new();
        let ancestor = collector.path(&normalized_ancestor, &mut hasher).unwrap();
        let descendant = collector.path(&descendant, &mut hasher).unwrap();
        let plan = test_derivation_plan();
        let definition = ResolvedOutput::default();
        let package = |name, path| {
            let mut bucket = analysis::Bucket::default();
            bucket.paths.push(path);
            Package::new_with_architecture(
                name,
                &plan.package,
                &definition,
                bucket,
                NonZeroU64::new(1).unwrap(),
                Architecture::X86_64,
                1,
            )
        };
        let packages = [package("ancestor", ancestor), package("descendant", descendant)];

        assert!(matches!(
            verify_unique_layout_targets(&packages),
            Err(Error::AncestorLayoutTarget {
                ref ancestor,
                ref descendant,
                ..
            }) if ancestor == "/bin" && descendant == "/bin/tool"
        ));
    }

    #[test]
    fn directory_normalized_ancestor_may_own_descendants() {
        let root = tempfile::tempdir().unwrap();
        let normalized_ancestor = root.path().join("usr/bin");
        let descendant = root.path().join("bin/tool");
        std::fs::create_dir_all(&normalized_ancestor).unwrap();
        std::fs::create_dir_all(descendant.parent().unwrap()).unwrap();
        std::fs::write(&descendant, b"payload").unwrap();
        let mut collector = collect::Collector::new(root.path());
        collector
            .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
            .unwrap();
        let mut hasher = stone::StoneDigestWriterHasher::new();
        let ancestor = collector.path(&normalized_ancestor, &mut hasher).unwrap();
        let descendant = collector.path(&descendant, &mut hasher).unwrap();
        let plan = test_derivation_plan();
        let definition = ResolvedOutput::default();
        let package = |name, path| {
            let mut bucket = analysis::Bucket::default();
            bucket.paths.push(path);
            Package::new_with_architecture(
                name,
                &plan.package,
                &definition,
                bucket,
                NonZeroU64::new(1).unwrap(),
                Architecture::X86_64,
                1,
            )
        };
        let packages = [package("ancestor", ancestor), package("descendant", descendant)];

        verify_unique_layout_targets(&packages).unwrap();
    }

    #[test]
    fn content_emission_preserves_the_primary_writer_error() {
        let path = Path::new("/verified/input");
        let write_result = Err(StoneWriteError::Io(io::Error::other("primary writer failure")));
        let verify_result = Err(collect::Error::TreeChanged {
            path: path.to_owned(),
            detail: "consequential short read",
        });

        assert!(matches!(
            finish_content_write(path, write_result, verify_result),
            Err(Error::StoneBinaryWriter { .. })
        ));
    }
}

#[cfg(test)]
pub(crate) fn test_derivation_plan() -> stone_recipe::derivation::DerivationPlan {
    static PLAN: std::sync::OnceLock<stone_recipe::derivation::DerivationPlan> = std::sync::OnceLock::new();

    PLAN.get_or_init(build_test_derivation_plan).clone()
}

#[cfg(test)]
pub(crate) fn set_test_compiler_cache(plan: &mut stone_recipe::derivation::DerivationPlan, enabled: bool) {
    use stone_recipe::derivation::{CompilerCacheRole, InputOrigin};

    let program = plan.toolchain_commands.compilers[0].command.program.clone();
    plan.execution.compiler_cache = enabled;
    plan.toolchain_commands.ccache = enabled.then(|| program.clone());
    plan.toolchain_commands.sccache = enabled.then_some(program);

    let request = plan
        .build_lock
        .requests
        .iter_mut()
        .find(|request| request.request == "binary(pkg-config)")
        .expect("test compiler-cache executable must be locked");
    request
        .origins
        .retain(|origin| !matches!(origin, InputOrigin::CompilerCache { .. }));
    if enabled {
        request.origins.extend([
            InputOrigin::CompilerCache {
                role: CompilerCacheRole::Ccache,
            },
            InputOrigin::CompilerCache {
                role: CompilerCacheRole::Sccache,
            },
        ]);
    }
    plan.build_lock.normalize();
}

#[cfg(test)]
fn test_evaluation(logical_name: &str, source: &str, explicit_inputs: &[u8]) -> gluon_config::EvaluationFingerprint {
    gluon_config::Evaluator::default()
        .evaluate_with_inputs::<i64>(&gluon_config::Source::new(logical_name, source), explicit_inputs)
        .expect("test provenance must be a real restricted evaluation")
        .fingerprint
}

#[cfg(test)]
fn build_test_derivation_plan() -> stone_recipe::derivation::DerivationPlan {
    use stone_recipe::build_policy::{AnalyzerKind, layers::BuildPolicyOperation};
    use stone_recipe::derivation::{
        BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuilderLayout, CompilerCommandPlan, DerivationProvenance,
        ExecutableCommandPlan, ExecutablePlan, ExecutionCredentials, InputOrigin, LockedIdentity, LockedOutput,
        LockedPackage, LockedRequest, OutputPlan, PackageIdentity, Platform, PolicyLayerProvenance, PolicyProvenance,
        PolicyTransitionProvenance, ProfileFragmentProvenance, RelationKind, RelationPlan, RepositorySnapshot,
        ToolchainCommandsPlan, policy_composition_identity, profile_aggregate_fingerprint,
    };

    const SOURCE_LOCK_BYTES: &[u8] = b"test source lock bytes";

    let profiles = vec![ProfileFragmentProvenance {
        logical_name: "default".to_owned(),
        evaluation: test_evaluation("profile.d/default.glu", "1", &[]),
    }];
    let layers = vec![PolicyLayerProvenance {
        name: "foundation".to_owned(),
        transitions: vec![PolicyTransitionProvenance {
            operation: BuildPolicyOperation::Add,
            origin: "default.glu".to_owned(),
            evaluation: test_evaluation("default.glu", "2", &[]),
        }],
    }];
    let policy_inputs = policy_composition_identity("aerynos", &layers);
    let provenance = DerivationProvenance {
        recipe: test_evaluation("stone.glu", "3", SOURCE_LOCK_BYTES),
        profiles,
        policy: PolicyProvenance {
            name: "aerynos".to_owned(),
            root: test_evaluation("policy.glu", "4", &policy_inputs),
            layers,
        },
    };
    let platform = Platform {
        architecture: "x86_64".to_owned(),
        vendor: "unknown".to_owned(),
        operating_system: "linux".to_owned(),
        abi: "gnu".to_owned(),
    };
    let identity = |name: &str| LockedIdentity {
        name: name.to_owned(),
        fingerprint: format!("{name}-fingerprint"),
    };
    let mut build_lock = BuildLock {
        schema_version: BUILD_LOCK_SCHEMA_VERSION,
        request_fingerprint: "request-fingerprint".to_owned(),
        repositories: vec![RepositorySnapshot {
            id: "test-repository".to_owned(),
            index_uri: "https://example.invalid/stone.index".to_owned(),
            snapshot: "test-repository-snapshot".to_owned(),
        }],
        requests: [
            "pkg-config",
            "python3",
            "llvm-objcopy",
            "llvm-strip",
            "objcopy",
            "strip",
        ]
        .into_iter()
        .map(|name| {
            let mut origins = vec![InputOrigin::Policy {
                source: "policy.glu".to_owned(),
                field: "build_root.base".to_owned(),
                index: 0,
            }];
            if name == "pkg-config" {
                origins.extend(
                    ToolchainCommandsPlan::COMPILER_ROLES
                        .into_iter()
                        .map(|role| InputOrigin::CompilerExecutable { role }),
                );
            }
            LockedRequest {
                request: format!("binary({name})"),
                package_id: "analyzer-tools-id".to_owned(),
                output: "out".to_owned(),
                origins,
            }
        })
        .collect(),
        packages: vec![LockedPackage {
            package_id: "analyzer-tools-id".to_owned(),
            name: "analyzer-tools".to_owned(),
            version: "1.0.0-1-1".to_owned(),
            architecture: "x86_64".to_owned(),
            repository: "test-repository".to_owned(),
            outputs: vec![LockedOutput { name: "out".to_owned() }],
            dependencies: Vec::new(),
        }],
        build_platform: platform.clone(),
        host_platform: platform.clone(),
        target_platform: platform,
        policy: LockedIdentity {
            name: provenance.policy.name.clone(),
            fingerprint: provenance.policy.root.sha256.clone(),
        },
        target: identity("x86_64"),
        profile: LockedIdentity {
            name: "profile".to_owned(),
            fingerprint: profile_aggregate_fingerprint(&provenance.profiles),
        },
        toolchain: identity("toolchain"),
        builder: identity("builder"),
    };
    build_lock.normalize();
    let mut plan = stone_recipe::derivation::DerivationPlan::new(
        PackageIdentity {
            name: "example".to_owned(),
            version: "1.2.3".to_owned(),
            source_release: 1,
            build_release: 1,
            homepage: "https://example.invalid".to_owned(),
            licenses: vec!["MPL-2.0".to_owned()],
            architecture: "x86_64".to_owned(),
        },
        build_lock,
        provenance,
    );
    plan.cast_version = "test-cast".to_owned();
    plan.cast_fingerprint = "sha256:test-cast-semantics".to_owned();
    plan.execution.executor = LockedIdentity {
        name: "test-executor".to_owned(),
        fingerprint: "test-executor-fingerprint".to_owned(),
    };
    plan.execution.credentials = ExecutionCredentials::IsolatedRoot;
    plan.source_lock_digest = plan.provenance.recipe.explicit_inputs_sha256.clone();
    plan.layout = BuilderLayout {
        hostname: "cast".to_owned(),
        guest_root: "/mason".to_owned(),
        artifacts_dir: "/mason/artefacts".to_owned(),
        build_dir: "/mason/build".to_owned(),
        source_dir: "/mason/sources".to_owned(),
        recipe_dir: "/mason/recipe".to_owned(),
        install_dir: "/mason/install".to_owned(),
        package_dir: "/mason/recipe/pkg".to_owned(),
        ccache_dir: "/mason/ccache".to_owned(),
        sccache_dir: "/mason/sccache".to_owned(),
        go_cache_dir: "/mason/gocache".to_owned(),
        go_mod_cache_dir: "/mason/gomodcache".to_owned(),
        cargo_cache_dir: "/mason/cargocache".to_owned(),
        zig_cache_dir: "/mason/zigcache".to_owned(),
    };
    plan.source_date_epoch = 1_700_000_000;
    plan.analysis.handlers = vec![
        AnalyzerKind::IgnoreBlocked,
        AnalyzerKind::Binary,
        AnalyzerKind::Elf,
        AnalyzerKind::PkgConfig,
        AnalyzerKind::Python,
        AnalyzerKind::CMake,
        AnalyzerKind::CompressMan,
        AnalyzerKind::IncludeAny,
    ];
    let analyzer_tool = |name: &str| ExecutablePlan {
        path: format!("/usr/bin/{name}"),
        requirement: RelationPlan {
            kind: RelationKind::Binary,
            name: name.to_owned(),
        },
    };
    plan.toolchain_commands.compilers = ToolchainCommandsPlan::COMPILER_ROLES
        .into_iter()
        .map(|role| CompilerCommandPlan {
            role,
            command: ExecutableCommandPlan {
                program: analyzer_tool("pkg-config"),
                args: Vec::new(),
            },
        })
        .collect();
    plan.analysis.tools.pkg_config = Some(analyzer_tool("pkg-config"));
    plan.analysis.tools.python = Some(analyzer_tool("python3"));
    plan.analysis.tools.strip = Some(analyzer_tool("llvm-strip"));
    plan.outputs = vec![OutputPlan {
        name: "out".to_owned(),
        package_name: "example".to_owned(),
        include_in_manifest: true,
        summary: None,
        description: None,
        provides_exclude: Vec::new(),
        runtime_exclude: Vec::new(),
        runtime_inputs: Vec::new(),
        conflicts: Vec::new(),
    }];
    plan.validate().unwrap();
    plan
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("artifact emission: {source}"))]
    Artifact { source: ArtifactError },
    #[snafu(display("{primary}; artifact rollback also failed: {cleanup}"))]
    ArtifactRollback {
        primary: Box<Error>,
        cleanup: ArtifactError,
    },
    #[snafu(display("stone binary writer"))]
    StoneBinaryWriter { source: StoneWriteError },
    #[snafu(display("manifest"))]
    Manifest { source: manifest::Error },
    #[snafu(display("io"))]
    Io { source: io::Error },
    #[snafu(display("verified package input {}: {source}", path.display()))]
    VerifiedInput { path: PathBuf, source: collect::Error },
    #[snafu(display("verified package inventory: {source}"))]
    VerifiedInventory { source: collect::Error },
    #[snafu(display("failed to reserve {requested} units for {resource}: {detail}"))]
    Allocation {
        resource: &'static str,
        requested: usize,
        detail: String,
    },
    #[snafu(display("size overflow while totaling {resource}"))]
    SizeOverflow { resource: &'static str },
    #[snafu(display(
        "package layout target {target} from {package} ({}) is reserved for Cast system metadata",
        path.display()
    ))]
    ReservedLayoutTarget {
        target: String,
        package: String,
        path: PathBuf,
    },
    #[snafu(display(
        "duplicate package layout target {target}: {first_package} ({}) and {second_package} ({})",
        first_path.display(),
        second_path.display()
    ))]
    DuplicateLayoutTarget {
        target: String,
        first_package: String,
        first_path: PathBuf,
        second_package: String,
        second_path: PathBuf,
    },
    #[snafu(display(
        "non-directory package layout target {ancestor} from {ancestor_package} ({}) is an ancestor of {descendant} from {descendant_package} ({})",
        ancestor_path.display(),
        descendant_path.display()
    ))]
    AncestorLayoutTarget {
        ancestor: String,
        ancestor_package: String,
        ancestor_path: PathBuf,
        descendant: String,
        descendant_package: String,
        descendant_path: PathBuf,
    },
}
