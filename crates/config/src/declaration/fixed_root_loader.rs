//! Typed loading for one language-neutral fixed declaration slot.

use std::{
    ffi::CString,
    fs::Metadata,
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::{
            ffi::OsStrExt as _,
            fs::MetadataExt as _,
        },
    },
    path::{Path, PathBuf},
};

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Source, SourceRoot,
};
use fs_err as fs;

use super::{
    DiscoveredRootDeclaration, FixedRootAuthorityError,
    FixedRootRevalidationPhase, LoadFixedRootDeclarationError,
    LoadedDeclaration, RootDeclarationSlot, TypedDeclarationEvaluatorSet,
};

/// Load the one registered-language declaration occupying `slot`.
///
/// The directory, selected name, and selected inode remain authenticated from
/// before the bounded read until evaluation and relative-import resolution
/// finish. A missing directory or a directory containing only unknown
/// extensions is represented by `Ok(None)`.
pub fn load_fixed_root_declaration<T, E>(
    directory: impl AsRef<Path>,
    slot: &RootDeclarationSlot,
    evaluators: &TypedDeclarationEvaluatorSet<T, E>,
) -> Result<
    Option<LoadedDeclaration<T, E::Identity>>,
    LoadFixedRootDeclarationError<E::Error>,
>
where
    E: DeclarationEvaluator<T>,
{
    let directory = directory.as_ref().to_owned();
    let retained_directory = match open_directory(&directory) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(LoadFixedRootDeclarationError::OpenDirectory {
                path: directory,
                source,
            });
        }
    };
    let directory_snapshot = snapshot(&retained_directory).map_err(|source| {
        LoadFixedRootDeclarationError::OpenDirectory {
            path: directory.clone(),
            source,
        }
    })?;
    let source_root = SourceRoot::from_directory(&directory, &retained_directory)
        .map_err(|source| LoadFixedRootDeclarationError::RetainSourceRoot {
            path: directory.clone(),
            source,
        })?;

    let discovered = slot
        .discover_at(
            &directory,
            &retained_directory,
            evaluators.languages(),
        )
        .map_err(|source| LoadFixedRootDeclarationError::Discovery {
            directory: directory.clone(),
            source,
        })?;
    let retained = discovered
        .as_ref()
        .map(|declaration| {
            RetainedDeclaration::open(
                &retained_directory,
                declaration.relative_path(),
                declaration.path(),
            )
        })
        .transpose()
        .map_err(|source| LoadFixedRootDeclarationError::RetainDeclaration {
            path: discovered
                .as_ref()
                .map_or_else(|| directory.clone(), |declaration| declaration.path().to_owned()),
            source,
        })?;
    let authority = FixedRootAuthority {
        directory,
        directory_snapshot,
        retained_directory,
        source_root,
        slot,
        languages: evaluators.languages(),
        discovered,
        retained,
    };

    authority.revalidate(FixedRootRevalidationPhase::BeforeRead)?;
    let Some(declaration) = authority.discovered.as_ref() else {
        return Ok(None);
    };
    let evaluator = evaluators.get(declaration.language()).ok_or_else(|| {
        LoadFixedRootDeclarationError::UnregisteredLanguage {
            path: declaration.path().to_owned(),
            extension: declaration.language().extension().to_owned(),
        }
    })?;

    let read = authority.source_root.load(
        declaration.relative_path(),
        evaluator.limits().max_source_bytes,
    );
    authority.revalidate(FixedRootRevalidationPhase::AfterRead)?;
    let source = read.map_err(|source| LoadFixedRootDeclarationError::Read {
        path: declaration.path().to_owned(),
        source,
    })?;
    let source = Source::new(
        declaration.logical_name(),
        source.text().to_owned(),
    );

    let rooted = evaluator.with_source_root(authority.source_root.clone());
    let result = rooted.evaluate(&source);
    // Authority wins over an evaluator failure so errors cannot bypass the
    // final retained-directory and slot checks.
    authority.revalidate(FixedRootRevalidationPhase::AfterEvaluation)?;
    let evaluation = result.map_err(|error| match error {
        DeclarationEvaluationError::Evaluation(source) => {
            LoadFixedRootDeclarationError::Evaluation {
                path: declaration.path().to_owned(),
                source,
            }
        }
        DeclarationEvaluationError::Conversion(source) => {
            LoadFixedRootDeclarationError::Conversion {
                path: declaration.path().to_owned(),
                source,
            }
        }
    })?;

    Ok(Some(LoadedDeclaration {
        logical_name: declaration.logical_name().to_owned(),
        path: declaration.path().to_owned(),
        language: declaration.language().clone(),
        value: evaluation.value,
        identity: evaluation.identity,
    }))
}

struct FixedRootAuthority<'a> {
    directory: PathBuf,
    directory_snapshot: NodeSnapshot,
    retained_directory: fs::File,
    source_root: SourceRoot,
    slot: &'a RootDeclarationSlot,
    languages: &'a super::RegisteredLanguages,
    discovered: Option<DiscoveredRootDeclaration>,
    retained: Option<RetainedDeclaration>,
}

impl FixedRootAuthority<'_> {
    fn revalidate<E>(
        &self,
        phase: FixedRootRevalidationPhase,
    ) -> Result<(), LoadFixedRootDeclarationError<E>> {
        self.revalidate_inner().map_err(|source| {
            LoadFixedRootDeclarationError::Revalidation {
                directory: self.directory.clone(),
                phase,
                source,
            }
        })
    }

    fn revalidate_inner(&self) -> Result<(), FixedRootAuthorityError> {
        self.source_root
            .verify_retained_directories()
            .map_err(|source| FixedRootAuthorityError::VerifySourceRoot {
                source,
            })?;
        let retained_snapshot = snapshot(&self.retained_directory).map_err(|source| {
            FixedRootAuthorityError::InspectDirectory {
                path: self.directory.clone(),
                source,
            }
        })?;
        if retained_snapshot != self.directory_snapshot {
            return Err(FixedRootAuthorityError::DirectoryChanged {
                path: self.directory.clone(),
            });
        }
        let public_directory = open_directory(&self.directory).map_err(|source| {
            FixedRootAuthorityError::InspectDirectory {
                path: self.directory.clone(),
                source,
            }
        })?;
        let public_snapshot = snapshot(&public_directory).map_err(|source| {
            FixedRootAuthorityError::InspectDirectory {
                path: self.directory.clone(),
                source,
            }
        })?;
        if public_snapshot != self.directory_snapshot {
            return Err(FixedRootAuthorityError::DirectoryChanged {
                path: self.directory.clone(),
            });
        }

        let actual = self
            .slot
            .discover_at(
                &self.directory,
                &self.retained_directory,
                self.languages,
            )
            .map_err(|source| FixedRootAuthorityError::DiscoverSlot {
                source,
            })?;
        if !same_declaration(self.discovered.as_ref(), actual.as_ref()) {
            return Err(FixedRootAuthorityError::SlotChanged {
                logical_name: self.slot.logical_name().to_owned(),
                expected: self
                    .discovered
                    .as_ref()
                    .map(|declaration| declaration.path().to_owned()),
                actual: actual
                    .as_ref()
                    .map(|declaration| declaration.path().to_owned()),
            });
        }

        if let Some(retained) = &self.retained {
            retained.verify(&self.retained_directory)?;
        }
        Ok(())
    }
}

struct RetainedDeclaration {
    path: PathBuf,
    relative_path: PathBuf,
    file: fs::File,
    snapshot: NodeSnapshot,
}

impl RetainedDeclaration {
    fn open(
        directory: &fs::File,
        relative_path: &Path,
        path: &Path,
    ) -> io::Result<Self> {
        let file = open_at(directory, relative_path, path)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixed declaration is not a regular file",
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            relative_path: relative_path.to_owned(),
            snapshot: NodeSnapshot::from_metadata(&metadata),
            file,
        })
    }

    fn verify(
        &self,
        directory: &fs::File,
    ) -> Result<(), FixedRootAuthorityError> {
        let retained_snapshot = snapshot(&self.file).map_err(|source| {
            FixedRootAuthorityError::InspectDeclaration {
                path: self.path.clone(),
                source,
            }
        })?;
        if retained_snapshot != self.snapshot {
            return Err(FixedRootAuthorityError::DeclarationChanged {
                path: self.path.clone(),
            });
        }
        let named = open_at(directory, &self.relative_path, &self.path).map_err(|source| {
            FixedRootAuthorityError::InspectDeclaration {
                path: self.path.clone(),
                source,
            }
        })?;
        let metadata = named.metadata().map_err(|source| {
            FixedRootAuthorityError::InspectDeclaration {
                path: self.path.clone(),
                source,
            }
        })?;
        if metadata.file_type().is_file()
            && NodeSnapshot::from_metadata(&metadata) == self.snapshot
        {
            Ok(())
        } else {
            Err(FixedRootAuthorityError::DeclarationChanged {
                path: self.path.clone(),
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    owner: u32,
    group: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl NodeSnapshot {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            owner: metadata.uid(),
            group: metadata.gid(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

fn open_directory(path: &Path) -> io::Result<fs::File> {
    let path_bytes = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixed declaration directory contains a NUL byte",
        )
    })?;
    // SAFETY: every open_how field accepts zero before explicit assignment.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(
        (libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_DIRECTORY
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK) as u32,
    );
    how.resolve = libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS;
    const MAX_INTERRUPTED_OPEN_RETRIES: usize = 1_024;
    let mut interruptions = 0usize;
    let descriptor = loop {
        // SAFETY: the path is NUL-terminated and how is initialized. Success
        // returns one fresh descriptor owned by this function.
        let descriptor = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                libc::AT_FDCWD,
                path_bytes.as_ptr(),
                &how,
                size_of::<libc::open_how>(),
            )
        };
        if descriptor != -1 {
            break descriptor;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        if interruptions == MAX_INTERRUPTED_OPEN_RETRIES {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "fixed declaration directory open exceeded interrupted retries",
            ));
        }
        interruptions += 1;
    };
    // SAFETY: openat2 returned a fresh descriptor owned by this function.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor as i32) };
    let directory = fs::File::from_parts(descriptor.into(), path.to_owned());
    let metadata = directory.metadata()?;
    if metadata.file_type().is_dir() {
        Ok(directory)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "fixed declaration path is not a real directory",
        ))
    }
}

fn open_at(
    directory: &fs::File,
    relative_path: &Path,
    diagnostic_path: &Path,
) -> io::Result<fs::File> {
    let descriptor = super::root_slot::open_beneath(
        directory.as_raw_fd(),
        relative_path,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )?;
    Ok(fs::File::from_parts(
        descriptor.into(),
        diagnostic_path.to_owned(),
    ))
}

fn snapshot(file: &fs::File) -> io::Result<NodeSnapshot> {
    file.metadata().map(|metadata| NodeSnapshot::from_metadata(&metadata))
}

fn same_declaration(
    expected: Option<&DiscoveredRootDeclaration>,
    actual: Option<&DiscoveredRootDeclaration>,
) -> bool {
    match (expected, actual) {
        (None, None) => true,
        (Some(expected), Some(actual)) => {
            expected.path() == actual.path()
                && expected.relative_path() == actual.relative_path()
                && expected.language() == actual.language()
                && expected.logical_name() == actual.logical_name()
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}
