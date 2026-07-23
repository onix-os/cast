//! Descriptor-rooted loading for the canonical authored system intent.

use std::{
    ffi::CString,
    fs::Metadata,
    io,
    os::{
        fd::AsRawFd as _,
        unix::ffi::OsStrExt as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::Path,
};

use config::declaration::{
    RootDeclarationSlot, TypedDeclarationEvaluatorSet,
};
use declarative_config::{DeclarationEvaluator, Source, SourceRoot};

use super::{
    LoadError, LoadedSystemModel,
    gluon::SystemIntentEvaluator,
    load_source,
};

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
    let evaluators = TypedDeclarationEvaluatorSet::new([
        SystemIntentEvaluator::default(),
    ])
    .expect("one validated system-intent adapter has no extension collision");
    let slot = RootDeclarationSlot::new("system", "system.glu")
        .expect("the canonical system-intent slot is valid");
    let discovered = slot
        .discover_at(directory_path, directory, evaluators.languages())
        .map_err(LoadError::RootedDiscovery)?;
    let Some(discovered) = discovered else {
        if slot
            .discover_at(directory_path, directory, evaluators.languages())
            .map_err(LoadError::RootedDiscovery)?
            .is_some()
        {
            return Err(LoadError::RootedSlotChanged(directory_path.to_owned()));
        }
        return Ok(None);
    };
    let source_path = discovered.path().to_owned();
    let relative_path = discovered.relative_path().to_owned();
    let Some(source_file) = open_source(directory, &relative_path, &source_path)? else {
        return Err(LoadError::RootedSourceChanged(source_path));
    };
    let expected = source_witness(&source_file, &source_path)?;

    after_rooted_system_source_retained();
    require_slot(
        directory_path,
        directory,
        &slot,
        &evaluators,
        &discovered,
    )?;
    require_named_source(
        directory,
        &source_file,
        &relative_path,
        &source_path,
        expected,
    )?;

    let source_root = SourceRoot::from_directory(directory_path, directory).map_err(evaluation)?;
    let evaluator = evaluators
        .get(discovered.language())
        .expect("the discovered system declaration has a registered adapter")
        .with_source_root(source_root.clone());
    let source = source_root
        .load(&relative_path, evaluator.limits().max_source_bytes)
        .map_err(evaluation)?;
    let source = Source::new(
        discovered.logical_name(),
        source.text().to_owned(),
    );
    source_root.verify_retained_directories().map_err(evaluation)?;
    let loaded = load_source(&source_path, source, &evaluator)?;
    source_root.verify_retained_directories().map_err(evaluation)?;

    require_slot(
        directory_path,
        directory,
        &slot,
        &evaluators,
        &discovered,
    )?;
    require_named_source(
        directory,
        &source_file,
        &relative_path,
        &source_path,
        expected,
    )?;

    Ok(Some(loaded))
}

fn require_slot(
    directory_path: &Path,
    directory: &std::fs::File,
    slot: &RootDeclarationSlot,
    evaluators: &TypedDeclarationEvaluatorSet<
        super::gluon::SystemIntentDeclaration,
        SystemIntentEvaluator,
    >,
    expected: &config::declaration::DiscoveredRootDeclaration,
) -> Result<(), LoadError> {
    let actual = slot
        .discover_at(directory_path, directory, evaluators.languages())
        .map_err(LoadError::RootedDiscovery)?;
    if actual.as_ref() == Some(expected) {
        Ok(())
    } else {
        Err(LoadError::RootedSlotChanged(directory_path.to_owned()))
    }
}

fn require_named_source(
    directory: &std::fs::File,
    retained: &std::fs::File,
    relative_path: &Path,
    path: &Path,
    expected: SourceWitness,
) -> Result<(), LoadError> {
    let named = open_source(directory, relative_path, path)?
        .ok_or_else(|| LoadError::RootedSourceChanged(path.to_owned()))?;
    if source_witness(retained, path)? == expected && source_witness(&named, path)? == expected {
        Ok(())
    } else {
        Err(LoadError::RootedSourceChanged(path.to_owned()))
    }
}

fn open_source(
    directory: &std::fs::File,
    relative_path: &Path,
    path: &Path,
) -> Result<Option<std::fs::File>, LoadError> {
    let flags = nix::libc::O_RDONLY
        | nix::libc::O_CLOEXEC
        | nix::libc::O_NOFOLLOW
        | nix::libc::O_NONBLOCK
        | nix::libc::O_NOCTTY;
    let relative_path = CString::new(relative_path.as_os_str().as_bytes())
        .map_err(|_| retain(path, io::Error::new(
            io::ErrorKind::InvalidInput,
            "system declaration path contains a NUL byte",
        )))?;
    match crate::linux_fs::openat2_file(
        directory.as_raw_fd(),
        &relative_path,
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
