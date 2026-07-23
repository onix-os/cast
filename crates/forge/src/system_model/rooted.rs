//! Descriptor-rooted loading for the canonical authored system intent.

use std::{
    fs::Metadata,
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::Path,
};

use gluon_config::{Evaluator, SourceRoot};

use super::{LoadError, LoadedSystemModel, load_source};

const SOURCE_NAME: &std::ffi::CStr = c"system.glu";
const SOURCE_MODE_MASK: u32 = 0o7777;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceWitness {
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
}

impl SourceWitness {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.permissions().mode() & SOURCE_MODE_MASK,
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

/// Load `system.glu` beneath one exact retained `etc/cast` directory.
///
/// `directory_path` is diagnostic only. Absence is established with one
/// descriptor-relative `openat2`, never `Path::exists`. A present source and
/// every relative import are read beneath the retained directory without
/// links or mount crossings. The initially selected source inode is checked
/// again after evaluation, so a replacement cannot become a successful load.
pub(crate) fn load_rooted(
    directory_path: &Path,
    directory: &std::fs::File,
) -> Result<Option<LoadedSystemModel>, LoadError> {
    let source_path = directory_path.join("system.glu");
    let Some(source_file) = open_source(directory, &source_path)? else {
        return Ok(None);
    };
    let expected = source_witness(&source_file, &source_path)?;

    after_rooted_system_source_retained();
    require_named_source(directory, &source_file, &source_path, expected)?;

    let source_root = SourceRoot::from_directory(directory_path, directory).map_err(evaluation)?;
    let evaluator = Evaluator::default().with_source_root(source_root.clone());
    let source = source_root
        .load(Path::new("system.glu"), evaluator.limits().max_source_bytes)
        .map_err(evaluation)?;
    source_root.verify_retained_directories().map_err(evaluation)?;
    let loaded = load_source(&source_path, source, &evaluator)?;
    source_root.verify_retained_directories().map_err(evaluation)?;

    require_named_source(directory, &source_file, &source_path, expected)?;

    Ok(Some(loaded))
}

fn require_named_source(
    directory: &std::fs::File,
    retained: &std::fs::File,
    path: &Path,
    expected: SourceWitness,
) -> Result<(), LoadError> {
    let named = open_source(directory, path)?.ok_or_else(|| LoadError::RootedSourceChanged(path.to_owned()))?;
    if source_witness(retained, path)? == expected && source_witness(&named, path)? == expected {
        Ok(())
    } else {
        Err(LoadError::RootedSourceChanged(path.to_owned()))
    }
}

fn open_source(directory: &std::fs::File, path: &Path) -> Result<Option<std::fs::File>, LoadError> {
    let flags = nix::libc::O_RDONLY
        | nix::libc::O_CLOEXEC
        | nix::libc::O_NOFOLLOW
        | nix::libc::O_NONBLOCK
        | nix::libc::O_NOCTTY;
    match crate::linux_fs::openat2_file(
        directory.as_raw_fd(),
        SOURCE_NAME,
        flags,
        0,
        crate::linux_fs::controlled_resolution(),
    ) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(retain(path, source)),
    }
}

fn source_witness(file: &std::fs::File, path: &Path) -> Result<SourceWitness, LoadError> {
    let metadata = file.metadata().map_err(|source| retain(path, source))?;
    let mode = metadata.permissions().mode() & SOURCE_MODE_MASK;
    // SAFETY: geteuid takes no arguments and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_file()
        || (metadata.uid() != effective_owner && metadata.uid() != 0)
        || mode & 0o7000 != 0
        || mode & 0o022 != 0
        || mode & 0o400 == 0
        || metadata.nlink() != 1
    {
        return Err(retain(
            path,
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "system intent is not a single-name, effective-user- or root-owned, non-writable regular file (uid={}, mode={mode:04o}, links={})",
                    metadata.uid(),
                    metadata.nlink()
                ),
            ),
        ));
    }
    crate::linux_fs::require_no_access_acl(file, path).map_err(|source| retain(path, source))?;
    Ok(SourceWitness::from_metadata(&metadata))
}

fn retain(path: &Path, source: io::Error) -> LoadError {
    LoadError::RetainRootedSource {
        path: path.to_owned(),
        source,
    }
}

fn evaluation(source: gluon_config::Diagnostic) -> LoadError {
    LoadError::Evaluation(super::gluon::EvaluationError::from(source))
}

#[cfg(test)]
std::thread_local! {
    static AFTER_ROOTED_SYSTEM_SOURCE_RETAINED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_after_rooted_system_source_retained(hook: impl FnOnce() + 'static) {
    AFTER_ROOTED_SYSTEM_SOURCE_RETAINED.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_rooted_system_source_retained() {
    AFTER_ROOTED_SYSTEM_SOURCE_RETAINED.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_rooted_system_source_retained() {}
