// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Collector-owned publication and admission of analyzer-generated paths.

#![allow(clippy::disallowed_types)]

use std::{
    ffi::{CString, OsStr, OsString},
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use nix::libc;
use stone::StoneDigestWriterHasher;

use super::{
    CollectionContext, Collector, Error, FileSnapshot, HASH_BUFFER_BYTES, NodeIdentity, PathInfo, VerifiedKind,
    WitnessGraph, c_name, changed, copy_os_string, enforce_u64_limit, metadata, open_entry, open_entry_handle, reserve,
};

const STAGE_ATTEMPTS: u64 = 128;
const STAGE_PREFIX: &str = ".mason-publication-";
static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub(crate) struct GeneratedTimes {
    pub(crate) accessed: SystemTime,
    pub(crate) modified: SystemTime,
}

#[derive(Debug)]
pub(crate) struct GeneratedArtifact {
    destination: PathBuf,
    kind: GeneratedKind,
    times: Option<GeneratedTimes>,
    analyze: bool,
}

#[derive(Debug)]
enum GeneratedKind {
    Regular { bytes: Vec<u8>, mode: u32 },
    Symlink { target: String },
}

impl GeneratedArtifact {
    pub(crate) fn regular(
        destination: PathBuf,
        bytes: Vec<u8>,
        mode: u32,
        times: Option<GeneratedTimes>,
        analyze: bool,
    ) -> Self {
        Self {
            destination,
            kind: GeneratedKind::Regular { bytes, mode },
            times,
            analyze,
        }
    }

    pub(crate) fn symlink(destination: PathBuf, target: String, times: Option<GeneratedTimes>, analyze: bool) -> Self {
        Self {
            destination,
            kind: GeneratedKind::Symlink { target },
            times,
            analyze,
        }
    }

    pub(crate) fn analyze(&self) -> bool {
        self.analyze
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublicationCheckpoint {
    BeforePublish,
    AfterPublish,
    BeforeAdmission,
    AfterAdmission,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
enum PublicationCheckpoint {
    BeforePublish,
    AfterPublish,
    BeforeAdmission,
    AfterAdmission,
}

impl Collector {
    pub(crate) fn publish_generated(
        &self,
        artifacts: &[GeneratedArtifact],
        hasher: &mut StoneDigestWriterHasher,
    ) -> Result<Vec<PathInfo>, Error> {
        publish_generated_with(self, artifacts, hasher, |_, _| Ok(()))
    }
}

#[cfg(test)]
pub(super) fn publish_generated_at_checkpoint(
    collector: &Collector,
    artifacts: &[GeneratedArtifact],
    hasher: &mut StoneDigestWriterHasher,
    mut hook: impl FnMut(PublicationCheckpoint, &Path) -> Result<(), Error>,
) -> Result<Vec<PathInfo>, Error> {
    publish_generated_with(collector, artifacts, hasher, |checkpoint, path| hook(checkpoint, path))
}

fn publish_generated_with(
    collector: &Collector,
    artifacts: &[GeneratedArtifact],
    hasher: &mut StoneDigestWriterHasher,
    mut hook: impl FnMut(PublicationCheckpoint, &Path) -> Result<(), Error>,
) -> Result<Vec<PathInfo>, Error> {
    if artifacts.is_empty() {
        return Ok(Vec::new());
    }

    let witness = collector.witness()?;
    let mut declarations = Vec::new();
    let mut display_paths = Vec::new();
    let mut order = Vec::new();
    let mut parent_components = 0usize;
    reserve(&mut declarations, artifacts.len(), "generated publication declarations")?;
    reserve(&mut display_paths, artifacts.len(), "generated publication paths")?;
    reserve(&mut order, artifacts.len(), "generated publication order")?;
    enforce_u64_limit(
        "generated package declarations",
        collector.limits.max_entries,
        artifacts.len() as u64,
        &collector.root,
    )?;
    let context = CollectionContext::detached(collector.limits, collector.deadline());
    for (index, artifact) in artifacts.iter().enumerate() {
        let display = collector.root.join(&artifact.destination);
        validate_destination(&context, &artifact.destination, &display)?;
        match &artifact.kind {
            GeneratedKind::Regular { bytes, .. } => enforce_u64_limit(
                "regular file bytes",
                collector.limits.max_file_bytes,
                u64::try_from(bytes.len()).map_err(|_| Error::ArithmeticOverflow {
                    resource: "regular file bytes",
                    path: display.clone(),
                })?,
                &display,
            )?,
            GeneratedKind::Symlink { target } => {
                if target.as_bytes().contains(&0) {
                    return Err(Error::InvalidPath {
                        path: display,
                        detail: "generated symlink target contains NUL",
                    });
                }
                super::enforce_usize_limit(
                    "symlink target bytes",
                    collector.limits.max_symlink_target_bytes,
                    target.len(),
                    &display,
                )?;
            }
        }
        parent_components = parent_components
            .checked_add(artifact.destination.components().count().saturating_sub(1))
            .ok_or(Error::ArithmeticOverflow {
                resource: "generated publication parent components",
                path: display.clone(),
            })?;
        declarations.push(artifact.destination.clone());
        display_paths.push(display);
        order.push(index);
    }
    order.sort_unstable_by(|left, right| declarations[*left].cmp(&declarations[*right]));
    for pair in order.windows(2) {
        if declarations[pair[0]] == declarations[pair[1]] {
            return Err(Error::DuplicateAdmission {
                path: collector.root.join(&declarations[pair[0]]),
            });
        }
        if declarations[pair[1]].starts_with(&declarations[pair[0]]) {
            return Err(Error::InvalidPath {
                path: collector.root.join(&declarations[pair[1]]),
                detail: "generated regular files and symlinks cannot be declaration ancestors",
            });
        }
    }

    let mut published = Vec::new();
    let mut created_directories = Vec::new();
    let mut output = Vec::new();
    let mut transitioned = false;
    reserve(&mut published, artifacts.len(), "generated publication ownership")?;
    reserve(
        &mut created_directories,
        parent_components,
        "generated publication directories",
    )?;
    reserve(&mut output, artifacts.len(), "generated path information")?;

    for (artifact, display) in artifacts.iter().zip(&display_paths) {
        let parent_relative = artifact.destination.parent().unwrap_or_else(|| Path::new(""));
        let parent = match ensure_parent(
            &witness,
            parent_relative,
            &mut created_directories,
            &mut transitioned,
            display,
        ) {
            Ok(parent) => parent,
            Err(primary) => {
                return Err(rollback_publication(
                    &witness,
                    &mut published,
                    &mut created_directories,
                    transitioned,
                    primary,
                    display,
                ));
            }
        };
        let name = artifact.destination.file_name().expect("validated publication name");
        let result = match &artifact.kind {
            GeneratedKind::Regular { bytes, mode } => publish_regular(
                &witness,
                parent,
                name,
                bytes,
                *mode,
                artifact.times,
                display,
                &mut published,
                &mut transitioned,
                &mut hook,
            ),
            GeneratedKind::Symlink { target } => publish_symlink(
                &witness,
                parent,
                name,
                target,
                artifact.times,
                display,
                &mut published,
                &mut transitioned,
                &mut hook,
            ),
        };
        if let Err(primary) = result {
            return Err(rollback_publication(
                &witness,
                &mut published,
                &mut created_directories,
                transitioned,
                primary,
                display,
            ));
        }
    }

    if let Err(primary) = hook(PublicationCheckpoint::BeforeAdmission, &collector.root) {
        return Err(rollback_publication(
            &witness,
            &mut published,
            &mut created_directories,
            transitioned,
            primary,
            &collector.root,
        ));
    }
    if let Err(primary) = witness.admit_paths(&declarations) {
        return Err(rollback_publication(
            &witness,
            &mut published,
            &mut created_directories,
            transitioned,
            primary,
            &collector.root,
        ));
    }

    if let Err(primary) = hook(PublicationCheckpoint::AfterAdmission, &collector.root) {
        witness.poison();
        return Err(Error::GeneratedPublicationCommitAmbiguous {
            path: collector.root.clone(),
            primary: Box::new(primary),
        });
    }

    for (((display, relative), owner), artifact) in
        display_paths.iter().zip(declarations).zip(&published).zip(artifacts)
    {
        let info = match collector.witnessed_path_info(&witness, display, relative, hasher) {
            Ok(info) => info,
            Err(primary) => {
                witness.poison();
                return Err(Error::GeneratedPublicationCommitAmbiguous {
                    path: display.clone(),
                    primary: Box::new(primary),
                });
            }
        };
        let verified = info.verified.as_ref().expect("published paths are verified");
        let content_matches = match (&artifact.kind, &verified.kind) {
            (GeneratedKind::Regular { bytes, .. }, VerifiedKind::Regular { hash }) => {
                let mut expected = StoneDigestWriterHasher::new();
                expected.update(bytes);
                expected.digest128() == *hash
            }
            (GeneratedKind::Symlink { target }, VerifiedKind::Symlink { target: actual }) => target == actual,
            _ => false,
        };
        if verified.snapshot.node != owner.node || owner.snapshot != Some(verified.snapshot) || !content_matches {
            witness.poison();
            return Err(Error::GeneratedPublicationCommitAmbiguous {
                path: display.clone(),
                primary: Box::new(changed(
                    display,
                    "published path identity, metadata, or declared content changed before admission completed",
                )),
            });
        }
        output.push(info);
    }
    Ok(output)
}

fn validate_destination(context: &CollectionContext, destination: &Path, display: &Path) -> Result<(), Error> {
    if destination.as_os_str().is_empty()
        || destination
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidPath {
            path: display.to_owned(),
            detail: "generated destination must be a non-empty normalized relative path",
        });
    }
    for component in destination.components() {
        let Component::Normal(name) = component else {
            unreachable!("generated destination components were validated above");
        };
        if name.to_str().is_none() {
            return Err(Error::NonUtf8Path {
                path: display.to_owned(),
            });
        }
        c_name(name, display)?;
    }
    context.admit_entry(destination, destination.components().count(), display)
}

#[derive(Debug)]
struct CreatedDirectory {
    relative: PathBuf,
    path: PathBuf,
    parent: File,
    name: OsString,
    node: NodeIdentity,
}

#[derive(Debug)]
struct PublishedNode {
    parent: File,
    name: OsString,
    node: NodeIdentity,
    snapshot: Option<FileSnapshot>,
    path: PathBuf,
}

fn ensure_parent(
    witness: &WitnessGraph,
    relative: &Path,
    created: &mut Vec<CreatedDirectory>,
    transitioned: &mut bool,
    display: &Path,
) -> Result<File, Error> {
    let mut directory = witness.anchor.open_directory(Path::new(""))?;
    let mut prefix = PathBuf::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(Error::InvalidPath {
                path: display.to_owned(),
                detail: "generated parent is not normalized",
            });
        };
        witness.deadline.check(display)?;
        prefix.push(name);
        let existing_id = witness.directory_id(&prefix);
        let owned = created.iter().find(|entry| entry.relative == prefix);
        match (existing_id, owned) {
            (Ok(id), None) => {
                let opened = open_entry(
                    &directory,
                    name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                    display,
                )?;
                let actual = metadata(&opened, "authenticate generated publication ancestor", display)?;
                if !actual.file_type().is_dir()
                    || NodeIdentity::from_metadata(&actual) != witness.directory_identity(id)?
                {
                    return Err(changed(display, "generated publication ancestor changed"));
                }
                directory = opened;
            }
            (Err(Error::UnwitnessedPath { .. }), Some(owned)) => {
                let opened = open_entry(
                    &directory,
                    name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                    display,
                )?;
                let actual = metadata(&opened, "authenticate created publication ancestor", display)?;
                if !actual.file_type().is_dir() || NodeIdentity::from_metadata(&actual) != owned.node {
                    return Err(changed(display, "created publication ancestor changed"));
                }
                directory = opened;
            }
            (Err(Error::UnwitnessedPath { .. }), None) => {
                let retained_parent = directory.try_clone().map_err(|source| Error::Io {
                    operation: "retain generated directory parent",
                    path: display.to_owned(),
                    source,
                })?;
                let owned_name = copy_os_string(name.as_bytes(), display)?;
                let c_name = c_name(name, display)?;
                // SAFETY: the parent descriptor and component are live; mkdirat
                // neither follows nor replaces the final component.
                if unsafe { libc::mkdirat(directory.as_raw_fd(), c_name.as_ptr(), 0o755) } == -1 {
                    return Err(Error::Io {
                        operation: "create generated package directory",
                        path: witness.anchor.path.join(&prefix),
                        source: io::Error::last_os_error(),
                    });
                }
                *transitioned = true;
                let opened = open_entry(
                    &directory,
                    name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                    display,
                )?;
                let actual = metadata(&opened, "authenticate created package directory", display)?;
                if !actual.file_type().is_dir() {
                    return Err(changed(display, "created publication ancestor is not a directory"));
                }
                let node = NodeIdentity::from_metadata(&actual);
                created.push(CreatedDirectory {
                    relative: prefix.clone(),
                    path: witness.anchor.path.join(&prefix),
                    parent: retained_parent,
                    name: owned_name,
                    node,
                });
                // SAFETY: opened pins the directory created above. Normalize
                // its declared package mode independently of the ambient
                // process umask before any child is published beneath it.
                if unsafe { libc::fchmod(opened.as_raw_fd(), 0o755) } == -1 {
                    return Err(Error::Io {
                        operation: "normalize generated package directory mode",
                        path: display.to_owned(),
                        source: io::Error::last_os_error(),
                    });
                }
                let normalized = metadata(&opened, "verify generated package directory mode", display)?;
                if !normalized.file_type().is_dir()
                    || NodeIdentity::from_metadata(&normalized) != node
                    || normalized.mode() & 0o7777 != 0o755
                {
                    return Err(changed(display, "created publication ancestor mode changed"));
                }
                let owner = created.last().expect("created directory ownership");
                sync_directory(&owner.parent, "sync generated package directory publication", display)?;
                sync_directory(&opened, "sync generated package directory", display)?;
                directory = opened;
            }
            (Ok(_), Some(_)) => return Err(changed(display, "generated directory was admitted concurrently")),
            (Err(error), _) => return Err(error),
        }
    }
    Ok(directory)
}

#[allow(clippy::too_many_arguments)]
fn publish_regular(
    witness: &WitnessGraph,
    parent: File,
    name: &OsStr,
    bytes: &[u8],
    mode: u32,
    times: Option<GeneratedTimes>,
    display: &Path,
    published: &mut Vec<PublishedNode>,
    transitioned: &mut bool,
    hook: &mut impl FnMut(PublicationCheckpoint, &Path) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut staged = open_private_regular(&parent, display)?;
    for chunk in bytes.chunks(HASH_BUFFER_BYTES) {
        witness.deadline.check(display)?;
        staged.write_all(chunk).map_err(|source| Error::Io {
            operation: "write private generated regular file",
            path: display.to_owned(),
            source,
        })?;
    }
    // SAFETY: staged is the live anonymous regular inode.
    if unsafe { libc::fchmod(staged.as_raw_fd(), mode & 0o7777) } == -1 {
        return Err(Error::Io {
            operation: "set generated regular file mode",
            path: display.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    if let Some(times) = times {
        set_file_times(&staged, times, display)?;
    }
    staged.sync_all().map_err(|source| Error::Io {
        operation: "sync private generated regular file",
        path: display.to_owned(),
        source,
    })?;
    let staged_metadata = metadata(&staged, "authenticate private generated regular file", display)?;
    if !staged_metadata.file_type().is_file()
        || staged_metadata.nlink() != 0
        || staged_metadata.len() != bytes.len() as u64
        || staged_metadata.mode() & 0o7777 != mode & 0o7777
    {
        return Err(changed(display, "private generated regular file metadata is invalid"));
    }
    let node = NodeIdentity::from_metadata(&staged_metadata);
    let owned_name = copy_os_string(name.as_bytes(), display)?;
    let owned_path = display.to_owned();
    hook(PublicationCheckpoint::BeforePublish, display)?;
    let name_c = c_name(name, display)?;
    // SAFETY: both descriptors and names are live. AT_EMPTY_PATH names the
    // retained anonymous inode, and linkat never replaces the destination.
    if unsafe {
        libc::linkat(
            staged.as_raw_fd(),
            c"".as_ptr(),
            parent.as_raw_fd(),
            name_c.as_ptr(),
            libc::AT_EMPTY_PATH,
        )
    } == -1
    {
        return Err(Error::Io {
            operation: "publish generated regular file without replacement",
            path: display.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    *transitioned = true;
    published.push(PublishedNode {
        parent,
        name: owned_name,
        node,
        snapshot: None,
        path: owned_path,
    });
    hook(PublicationCheckpoint::AfterPublish, display)?;
    let owner = published.last_mut().expect("published regular ownership");
    let opened = open_entry_handle(&owner.parent, &owner.name, display)?;
    let actual = metadata(&opened, "verify published generated regular file", display)?;
    if !actual.file_type().is_file()
        || actual.nlink() != 1
        || NodeIdentity::from_metadata(&actual) != owner.node
        || actual.len() != bytes.len() as u64
    {
        return Err(changed(display, "published generated regular file changed"));
    }
    owner.snapshot = Some(FileSnapshot::from_metadata(&actual));
    sync_directory(&owner.parent, "sync generated regular publication", display)
}

#[allow(clippy::too_many_arguments)]
fn publish_symlink(
    witness: &WitnessGraph,
    parent: File,
    name: &OsStr,
    target: &str,
    times: Option<GeneratedTimes>,
    display: &Path,
    published: &mut Vec<PublishedNode>,
    transitioned: &mut bool,
    hook: &mut impl FnMut(PublicationCheckpoint, &Path) -> Result<(), Error>,
) -> Result<(), Error> {
    let target_c = CString::new(target).map_err(|_| Error::InvalidPath {
        path: display.to_owned(),
        detail: "generated symlink target contains NUL",
    })?;
    let owned_name = copy_os_string(name.as_bytes(), display)?;
    let owned_path = display.to_owned();
    let destination_c = c_name(name, display)?;
    let (stage_name, stage_c, _stage_handle, node) =
        create_staged_symlink(witness, &parent, &target_c, target, display, transitioned)?;
    if let Some(times) = times
        && let Err(primary) = set_symlink_times(&parent, &stage_c, times, display)
    {
        return match unlink_owned(&parent, &stage_name, node, false, display) {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(Error::GeneratedPublicationRollback {
                path: display.to_owned(),
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }),
        };
    }
    if let Err(primary) = hook(PublicationCheckpoint::BeforePublish, display) {
        return match unlink_owned(&parent, &stage_name, node, false, display) {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(Error::GeneratedPublicationRollback {
                path: display.to_owned(),
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }),
        };
    }
    // SAFETY: names and parent are live. RENAME_NOREPLACE preserves an existing
    // destination and atomically publishes the already-authenticated symlink.
    if unsafe {
        libc::renameat2(
            parent.as_raw_fd(),
            stage_c.as_ptr(),
            parent.as_raw_fd(),
            destination_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == -1
    {
        let primary = Error::Io {
            operation: "publish generated symlink without replacement",
            path: display.to_owned(),
            source: io::Error::last_os_error(),
        };
        return match unlink_owned(&parent, &stage_name, node, false, display) {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(Error::GeneratedPublicationRollback {
                path: display.to_owned(),
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }),
        };
    }
    published.push(PublishedNode {
        parent,
        name: owned_name,
        node,
        snapshot: None,
        path: owned_path,
    });
    hook(PublicationCheckpoint::AfterPublish, display)?;
    let owner = published.last_mut().expect("published symlink ownership");
    let opened = open_entry_handle(&owner.parent, &owner.name, display)?;
    let actual = metadata(&opened, "verify published generated symlink", display)?;
    if !actual.file_type().is_symlink()
        || NodeIdentity::from_metadata(&actual) != owner.node
        || read_symlink(&opened, target.len(), display)? != target
    {
        return Err(changed(display, "published generated symlink changed"));
    }
    owner.snapshot = Some(FileSnapshot::from_metadata(&actual));
    sync_directory(&owner.parent, "sync generated symlink publication", display)
}

fn open_private_regular(parent: &File, display: &Path) -> Result<File, Error> {
    // SAFETY: parent and the static component are live.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c".".as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if descriptor == -1 {
        return Err(Error::Io {
            operation: "create private generated regular file",
            path: display.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) }.into())
}

fn create_staged_symlink(
    witness: &WitnessGraph,
    parent: &File,
    target_c: &CString,
    target: &str,
    display: &Path,
    transitioned: &mut bool,
) -> Result<(OsString, CString, File, NodeIdentity), Error> {
    let process = unsafe { libc::getpid() } as u32;
    let first = NEXT_STAGE.fetch_add(STAGE_ATTEMPTS, Ordering::Relaxed);
    let mut last = None;
    for attempt in 0..STAGE_ATTEMPTS {
        witness.deadline.check(display)?;
        let name = OsString::from(format!(
            "{STAGE_PREFIX}{process:08x}-{:016x}",
            first.wrapping_add(attempt)
        ));
        let name_c = c_name(&name, display)?;
        // SAFETY: target, name, and parent are live; symlinkat never replaces.
        if unsafe { libc::symlinkat(target_c.as_ptr(), parent.as_raw_fd(), name_c.as_ptr()) } == 0 {
            *transitioned = true;
            let handle = open_entry_handle(parent, &name, display)?;
            let staged = metadata(&handle, "inspect staged generated symlink", display)?;
            let node = NodeIdentity::from_metadata(&staged);
            let validation: Result<(), Error> = if !staged.file_type().is_symlink() {
                Err(changed(display, "staged generated symlink changed type"))
            } else {
                match read_symlink(&handle, target.len(), display) {
                    Ok(actual) if actual == target => return Ok((name, name_c, handle, node)),
                    Ok(_) => Err(changed(display, "staged generated symlink target changed")),
                    Err(error) => Err(error),
                }
            };
            let primary = validation.expect_err("invalid staged symlink validation");
            return match unlink_owned(parent, &name, node, false, display) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(Error::GeneratedPublicationRollback {
                    path: display.to_owned(),
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            last = Some(error);
            continue;
        }
        return Err(Error::Io {
            operation: "create staged generated symlink",
            path: display.to_owned(),
            source: error,
        });
    }
    Err(Error::Io {
        operation: "create collision-free staged generated symlink",
        path: display.to_owned(),
        source: last.unwrap_or_else(|| io::Error::from_raw_os_error(libc::EEXIST)),
    })
}

fn read_symlink(handle: &File, expected_len: usize, display: &Path) -> Result<String, Error> {
    let capacity = expected_len.checked_add(1).ok_or(Error::ArithmeticOverflow {
        resource: "generated symlink target bytes",
        path: display.to_owned(),
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(|source| Error::Allocation {
        resource: "generated symlink target bytes",
        requested: capacity,
        detail: source.to_string(),
    })?;
    bytes.resize(capacity, 0);
    // SAFETY: handle pins a symlink and bytes is writable.
    let read = unsafe { libc::readlinkat(handle.as_raw_fd(), c"".as_ptr(), bytes.as_mut_ptr().cast(), bytes.len()) };
    if read == -1 {
        return Err(Error::Io {
            operation: "read generated symlink target",
            path: display.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ArithmeticOverflow {
        resource: "generated symlink target bytes",
        path: display.to_owned(),
    })?;
    if read != expected_len {
        return Err(changed(display, "generated symlink target length changed"));
    }
    bytes.truncate(read);
    String::from_utf8(bytes).map_err(|_| Error::NonUtf8SymlinkTarget {
        path: display.to_owned(),
    })
}

fn set_file_times(file: &File, times: GeneratedTimes, display: &Path) -> Result<(), Error> {
    let times = timespecs(times);
    // SAFETY: file is live and points to two initialized timespec values.
    if unsafe { libc::futimens(file.as_raw_fd(), times.as_ptr()) } == -1 {
        Err(Error::Io {
            operation: "set generated regular file times",
            path: display.to_owned(),
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn set_symlink_times(parent: &File, name: &CString, times: GeneratedTimes, display: &Path) -> Result<(), Error> {
    let times = timespecs(times);
    // SAFETY: parent, name, and initialized timespecs are live.
    if unsafe {
        libc::utimensat(
            parent.as_raw_fd(),
            name.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == -1
    {
        Err(Error::Io {
            operation: "set generated symlink times",
            path: display.to_owned(),
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn timespecs(times: GeneratedTimes) -> [libc::timespec; 2] {
    let accessed = filetime::FileTime::from_system_time(times.accessed);
    let modified = filetime::FileTime::from_system_time(times.modified);
    [
        libc::timespec {
            tv_sec: accessed.seconds() as libc::time_t,
            tv_nsec: accessed.nanoseconds() as libc::c_long,
        },
        libc::timespec {
            tv_sec: modified.seconds() as libc::time_t,
            tv_nsec: modified.nanoseconds() as libc::c_long,
        },
    ]
}

fn rollback_publication(
    witness: &WitnessGraph,
    published: &mut Vec<PublishedNode>,
    created: &mut Vec<CreatedDirectory>,
    transitioned: bool,
    primary: Error,
    display: &Path,
) -> Error {
    let cleanup = cleanup_publication(published, created);
    if transitioned {
        witness.poison();
    }
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => Error::GeneratedPublicationRollback {
            path: display.to_owned(),
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        },
    }
}

fn cleanup_publication(published: &mut Vec<PublishedNode>, created: &mut Vec<CreatedDirectory>) -> Result<(), Error> {
    let mut first = None;
    while let Some(node) = published.pop() {
        if let Err(error) = unlink_owned(&node.parent, &node.name, node.node, false, &node.path) {
            first.get_or_insert(error);
        }
    }
    while let Some(directory) = created.pop() {
        if let Err(error) = unlink_owned(
            &directory.parent,
            &directory.name,
            directory.node,
            true,
            &directory.path,
        ) {
            first.get_or_insert(error);
        }
    }
    first.map_or(Ok(()), Err)
}

fn unlink_owned(
    parent: &File,
    name: &OsStr,
    expected: NodeIdentity,
    directory: bool,
    display: &Path,
) -> Result<(), Error> {
    let handle = open_entry_handle(parent, name, display)?;
    let actual = metadata(&handle, "authenticate generated path before rollback", display)?;
    if NodeIdentity::from_metadata(&actual) != expected || actual.file_type().is_dir() != directory {
        return Err(changed(display, "generated path ownership changed before rollback"));
    }
    let name = c_name(name, display)?;
    // SAFETY: parent and name are live; unlinkat does not follow the final component.
    if unsafe {
        libc::unlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    } == -1
    {
        return Err(Error::Io {
            operation: "rollback generated package path",
            path: display.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    sync_directory(parent, "sync generated publication rollback", display)
}

fn sync_directory(directory: &File, operation: &'static str, display: &Path) -> Result<(), Error> {
    directory.sync_all().map_err(|source| Error::Io {
        operation,
        path: display.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        os::unix::{ffi::OsStringExt, fs::symlink},
    };

    use fs_err as fs;
    use stone_recipe::derivation::PathRuleKind;

    use super::*;
    use crate::package::collect::{CollectionLimits, Error};

    fn make_collector(root: &Path, limits: CollectionLimits) -> Collector {
        let mut collector = Collector::new_with_limits(root, limits);
        collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
        collector
    }

    #[test]
    fn regular_publication_accepts_n_and_rejects_n_plus_one_before_mutation() {
        let exact = tempfile::tempdir().unwrap();
        let mut limits = CollectionLimits::default();
        limits.max_file_bytes = 8;
        let collector = make_collector(exact.path(), limits);
        let artifact =
            GeneratedArtifact::regular(PathBuf::from("nested/output"), b"12345678".to_vec(), 0o640, None, false);
        let info = collector
            .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(fs::read(&info.path).unwrap(), b"12345678");
        assert_eq!(fs::metadata(&info.path).unwrap().mode() & 0o7777, 0o640);
        info.verify_unchanged().unwrap();
        collector.seal().unwrap();

        let over = tempfile::tempdir().unwrap();
        let collector = make_collector(over.path(), limits);
        let artifact = GeneratedArtifact::regular(
            PathBuf::from("nested/output"),
            b"123456789".to_vec(),
            0o644,
            None,
            false,
        );
        assert!(matches!(
            collector.publish_generated(&[artifact], &mut StoneDigestWriterHasher::new()),
            Err(Error::LimitExceeded {
                resource: "regular file bytes",
                limit: 8,
                actual: 9,
                ..
            })
        ));
        assert!(!over.path().join("nested").exists());
        collector.seal().unwrap();
    }

    #[test]
    fn publication_normalizes_generated_directory_mode_under_adverse_umask() {
        const CHILD: &str = "MASON_GENERATED_PUBLICATION_UMASK_CHILD";
        const TEST: &str =
            "package::collect::publication::tests::publication_normalizes_generated_directory_mode_under_adverse_umask";

        // umask is process-global. Isolate it from every other unit test.
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .arg(TEST)
                .arg("--exact")
                .arg("--nocapture")
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "adverse-umask child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let collector = make_collector(root.path(), CollectionLimits::default());
        let artifact = GeneratedArtifact::regular(PathBuf::from("nested/output"), b"data".to_vec(), 0o640, None, false);
        // SAFETY: this is the sole test selected in the isolated child.
        let previous = unsafe { libc::umask(0o277) };
        let result = collector.publish_generated(&[artifact], &mut StoneDigestWriterHasher::new());
        // SAFETY: restore the child mask before assertions can panic.
        unsafe { libc::umask(previous) };

        let info = result.unwrap().pop().unwrap();
        assert_eq!(fs::metadata(root.path().join("nested")).unwrap().mode() & 0o7777, 0o755);
        assert_eq!(fs::metadata(&info.path).unwrap().mode() & 0o7777, 0o640);
        collector.seal().unwrap();
    }

    #[test]
    fn symlink_publication_accepts_n_and_rejects_n_plus_one() {
        let exact = tempfile::tempdir().unwrap();
        let mut limits = CollectionLimits::default();
        limits.max_symlink_target_bytes = 8;
        let collector = make_collector(exact.path(), limits);
        let artifact = GeneratedArtifact::symlink(PathBuf::from("links/output"), "12345678".to_owned(), None, false);
        let info = collector
            .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(fs::read_link(&info.path).unwrap(), Path::new("12345678"));
        info.verify_unchanged().unwrap();
        collector.seal().unwrap();

        let over = tempfile::tempdir().unwrap();
        let collector = make_collector(over.path(), limits);
        let artifact = GeneratedArtifact::symlink(PathBuf::from("links/output"), "123456789".to_owned(), None, false);
        assert!(matches!(
            collector.publish_generated(&[artifact], &mut StoneDigestWriterHasher::new()),
            Err(Error::LimitExceeded {
                resource: "symlink target bytes",
                limit: 8,
                actual: 9,
                ..
            })
        ));
        assert!(!over.path().join("links").exists());
        collector.seal().unwrap();
    }

    #[test]
    fn publication_rejects_non_relative_or_unrepresentable_destinations_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        let collector = make_collector(root.path(), CollectionLimits::default());
        let invalid = [
            PathBuf::new(),
            PathBuf::from("/absolute"),
            PathBuf::from("../escape"),
            PathBuf::from(OsString::from_vec(b"nul\0name".to_vec())),
            PathBuf::from(OsString::from_vec(vec![0xff])),
        ];

        for destination in invalid {
            let artifact = GeneratedArtifact::regular(destination, b"data".to_vec(), 0o644, None, false);
            assert!(
                collector
                    .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
                    .is_err()
            );
        }

        let conflicting = [
            GeneratedArtifact::regular(PathBuf::from("parent"), b"data".to_vec(), 0o644, None, false),
            GeneratedArtifact::symlink(PathBuf::from("parent/child"), "target".to_owned(), None, false),
        ];
        assert!(
            collector
                .publish_generated(&conflicting, &mut StoneDigestWriterHasher::new())
                .is_err()
        );

        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        collector.seal().unwrap();
    }

    #[test]
    fn publication_never_traverses_a_witnessed_symlink_parent() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let parent = root.path().join("nested");
        symlink(outside.path(), &parent).unwrap();
        let collector = make_collector(root.path(), CollectionLimits::default());
        let artifact = GeneratedArtifact::regular(PathBuf::from("nested/output"), b"data".to_vec(), 0o644, None, false);

        assert!(
            collector
                .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
                .is_err()
        );
        assert_eq!(fs::read_link(&parent).unwrap(), outside.path());
        assert!(!outside.path().join("output").exists());
        collector.seal().unwrap();
    }

    #[test]
    fn regular_publication_never_clobbers_an_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("output");
        fs::write(&path, b"original").unwrap();
        let collector = make_collector(root.path(), CollectionLimits::default());
        collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
        let artifact = GeneratedArtifact::regular(PathBuf::from("output"), b"replacement".to_vec(), 0o644, None, false);
        assert!(
            collector
                .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"original");
        collector.seal().unwrap();
    }

    #[test]
    fn symlink_publication_never_clobbers_an_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("output");
        symlink("original", &path).unwrap();
        let collector = make_collector(root.path(), CollectionLimits::default());
        let artifact = GeneratedArtifact::symlink(PathBuf::from("output"), "replacement".to_owned(), None, false);

        assert!(
            collector
                .publish_generated(&[artifact], &mut StoneDigestWriterHasher::new())
                .is_err()
        );
        assert_eq!(fs::read_link(&path).unwrap(), Path::new("original"));
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            [OsString::from("output")]
        );
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn post_publication_substitution_is_not_unlinked_and_poisons_inventory() {
        let root = tempfile::tempdir().unwrap();
        let collector = make_collector(root.path(), CollectionLimits::default());
        let artifact = GeneratedArtifact::regular(PathBuf::from("output"), b"generated".to_vec(), 0o644, None, false);
        let result = publish_generated_at_checkpoint(
            &collector,
            &[artifact],
            &mut StoneDigestWriterHasher::new(),
            |checkpoint, path| {
                if checkpoint == PublicationCheckpoint::AfterPublish {
                    fs::remove_file(path).unwrap();
                    symlink("attacker", path).unwrap();
                }
                Ok(())
            },
        );
        assert!(matches!(result, Err(Error::GeneratedPublicationRollback { .. })));
        assert_eq!(
            fs::read_link(root.path().join("output")).unwrap(),
            Path::new("attacker")
        );
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn same_inode_content_race_cannot_be_admitted_as_declared_bytes() {
        let root = tempfile::tempdir().unwrap();
        let collector = make_collector(root.path(), CollectionLimits::default());
        let artifact = GeneratedArtifact::regular(PathBuf::from("output"), b"generated".to_vec(), 0o644, None, false);
        let result = publish_generated_at_checkpoint(
            &collector,
            &[artifact],
            &mut StoneDigestWriterHasher::new(),
            |checkpoint, path| {
                if checkpoint == PublicationCheckpoint::AfterPublish {
                    fs::write(path, b"attacker!").unwrap();
                }
                Ok(())
            },
        );

        assert!(matches!(result, Err(Error::GeneratedPublicationCommitAmbiguous { .. })));
        assert_eq!(fs::read(root.path().join("output")).unwrap(), b"attacker!");
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn partial_batch_failure_rolls_back_owned_nodes_and_poisons_inventory() {
        let root = tempfile::tempdir().unwrap();
        let collector = make_collector(root.path(), CollectionLimits::default());
        let artifacts = [
            GeneratedArtifact::regular(PathBuf::from("nested/first"), b"first".to_vec(), 0o644, None, false),
            GeneratedArtifact::regular(PathBuf::from("nested/second"), b"second".to_vec(), 0o644, None, false),
        ];
        let result = publish_generated_at_checkpoint(
            &collector,
            &artifacts,
            &mut StoneDigestWriterHasher::new(),
            |checkpoint, path| {
                if checkpoint == PublicationCheckpoint::BeforePublish && path.ends_with("second") {
                    return Err(changed(path, "injected publication failure"));
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!root.path().join("nested").exists());
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }

    #[test]
    fn failure_after_admission_is_ambiguous_and_poisons_without_deleting_committed_path() {
        let root = tempfile::tempdir().unwrap();
        let collector = make_collector(root.path(), CollectionLimits::default());
        let artifact = GeneratedArtifact::regular(PathBuf::from("output"), b"generated".to_vec(), 0o644, None, false);
        let result = publish_generated_at_checkpoint(
            &collector,
            &[artifact],
            &mut StoneDigestWriterHasher::new(),
            |checkpoint, path| {
                if checkpoint == PublicationCheckpoint::AfterAdmission {
                    return Err(changed(path, "injected post-admission failure"));
                }
                Ok(())
            },
        );
        assert!(matches!(result, Err(Error::GeneratedPublicationCommitAmbiguous { .. })));
        assert_eq!(fs::read(root.path().join("output")).unwrap(), b"generated");
        assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
    }
}
