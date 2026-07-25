use std::{
    collections::BTreeMap,
    ffi::CString,
    fmt,
    io::Read,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use fs_err::File;

use crate::{Diagnostic, LimitKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    logical_name: String,
    text: String,
}

impl Source {
    pub fn new(logical_name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            logical_name: logical_name.into(),
            text: text.into(),
        }
    }

    pub fn logical_name(&self) -> &str {
        &self.logical_name
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone)]
pub struct SourceRoot {
    canonical: PathBuf,
    directory: Arc<File>,
    identity: SourceRootIdentity,
    prohibit_mount_crossing: bool,
    retained_root: Option<SourceNodeSnapshot>,
    retained_directories: Option<Arc<Mutex<BTreeMap<PathBuf, RetainedDirectory>>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceRootIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceNodeSnapshot {
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

impl SourceNodeSnapshot {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
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

#[derive(Debug)]
struct RetainedDirectory {
    descriptor: File,
    expected: SourceNodeSnapshot,
}

impl SourceRootIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

impl fmt::Debug for SourceRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRoot")
            .field("canonical", &self.canonical)
            .finish()
    }
}

impl PartialEq for SourceRoot {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl Eq for SourceRoot {}

impl SourceRoot {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, Diagnostic> {
        Self::new_with_hook(path.as_ref(), || {})
    }

    fn new_with_hook(path: &Path, after_descriptor_open: impl FnOnce()) -> Result<Self, Diagnostic> {
        // Open the authored path first. Normal symlinks are permitted here to
        // preserve SourceRoot's historical root-path semantics, but magic
        // links are never accepted. Every descendant load is stricter.
        let directory = openat2_file(
            libc::AT_FDCWD,
            path.as_os_str().as_bytes(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            libc::RESOLVE_NO_MAGICLINKS,
            path,
        )
        .map_err(|error| Diagnostic::io(Some(path.display().to_string()), error))?;
        let metadata = directory
            .metadata()
            .map_err(|error| Diagnostic::io(Some(path.display().to_string()), error))?;
        if !metadata.file_type().is_dir() {
            return Err(Diagnostic::io(
                Some(path.display().to_string()),
                std::io::Error::new(std::io::ErrorKind::NotADirectory, "source root is not a directory"),
            ));
        }
        let identity = SourceRootIdentity::from_metadata(&metadata);

        after_descriptor_open();
        let canonical = path
            .canonicalize()
            .map_err(|error| Diagnostic::io(Some(path.display().to_string()), error))?;
        let canonical_directory = openat2_file(
            libc::AT_FDCWD,
            canonical.as_os_str().as_bytes(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
            &canonical,
        )
        .map_err(|error| Diagnostic::io(Some(canonical.display().to_string()), error))?;
        let canonical_metadata = canonical_directory
            .metadata()
            .map_err(|error| Diagnostic::io(Some(canonical.display().to_string()), error))?;
        if !canonical_metadata.file_type().is_dir() {
            return Err(Diagnostic::io(
                Some(canonical.display().to_string()),
                std::io::Error::new(std::io::ErrorKind::NotADirectory, "source root is not a directory"),
            ));
        }
        if SourceRootIdentity::from_metadata(&canonical_metadata) != identity {
            return Err(Diagnostic::io(
                Some(path.display().to_string()),
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "source root changed while it was being opened",
                ),
            ));
        }
        Ok(Self {
            canonical,
            directory: Arc::new(directory),
            identity,
            prohibit_mount_crossing: false,
            retained_root: None,
            retained_directories: None,
        })
    }

    /// Retain an already-selected directory as a configuration source root.
    ///
    /// `path` is used only for diagnostics. All source and import resolution
    /// starts from an owned duplicate of `directory`, never from that path,
    /// and cannot follow links, escape the directory, or cross a mount.
    pub fn from_directory(path: impl AsRef<Path>, directory: &impl AsRawFd) -> Result<Self, Diagnostic> {
        let path = path.as_ref();
        let retained = openat2_file(
            directory.as_raw_fd(),
            b".",
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV,
            path,
        )
        .map_err(|error| Diagnostic::io(Some(path.display().to_string()), error))?;
        let metadata = retained
            .metadata()
            .map_err(|error| Diagnostic::io(Some(path.display().to_string()), error))?;
        if !metadata.file_type().is_dir() {
            return Err(Diagnostic::io(
                Some(path.display().to_string()),
                std::io::Error::new(std::io::ErrorKind::NotADirectory, "source root is not a directory"),
            ));
        }

        Ok(Self {
            canonical: path.to_owned(),
            directory: Arc::new(retained),
            identity: SourceRootIdentity::from_metadata(&metadata),
            prohibit_mount_crossing: true,
            retained_root: Some(SourceNodeSnapshot::from_metadata(&metadata)),
            retained_directories: Some(Arc::new(Mutex::new(BTreeMap::new()))),
        })
    }

    pub fn path(&self) -> &Path {
        &self.canonical
    }

    /// Verify every intermediate directory retained while descriptor-rooted
    /// sources and imports were loaded.
    ///
    /// Path-based roots do not retain this additional state, preserving their
    /// existing behavior. Descriptor-rooted callers should verify after the
    /// complete decode so a directory-chain substitution cannot outlive an
    /// otherwise stable source-file read.
    pub fn verify_retained_directories(&self) -> Result<(), Diagnostic> {
        let Some(retained) = &self.retained_directories else {
            return Ok(());
        };
        let retained = retained
            .lock()
            .map_err(|_| Diagnostic::internal("retained source-directory witnesses were poisoned"))?;
        self.verify_retained_directories_locked(&retained)
            .map_err(|(relative, error)| Diagnostic::io(Some(relative.to_string_lossy().replace('\\', "/")), error))
    }

    pub fn load(&self, relative: impl AsRef<Path>, max_bytes: usize) -> Result<Source, Diagnostic> {
        self.load_inner(relative.as_ref(), max_bytes, LimitKind::SourceSize, false)
    }

    pub(crate) fn load_import(&self, relative: &Path, max_bytes: usize) -> Result<Source, Diagnostic> {
        self.load_inner(relative, max_bytes, LimitKind::ImportedFileSize, true)
    }

    fn load_inner(
        &self,
        relative: &Path,
        max_bytes: usize,
        limit_kind: LimitKind,
        is_import: bool,
    ) -> Result<Source, Diagnostic> {
        let relative = normalize_relative(relative, is_import)?;
        let logical_name = relative.to_string_lossy().replace('\\', "/");
        self.retain_intermediate_directories(&relative)
            .map_err(|error| load_error(&logical_name, error, is_import))?;
        let mut resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS;
        if self.prohibit_mount_crossing {
            resolve |= libc::RESOLVE_NO_XDEV;
        }
        let mut file = openat2_file(
            self.directory.as_raw_fd(),
            relative.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
            resolve,
            &self.canonical.join(&relative),
        )
        .map_err(|error| load_error(&logical_name, error, is_import))?;
        let metadata = file
            .metadata()
            .map_err(|error| load_error(&logical_name, error, is_import))?;
        if !metadata.file_type().is_file() {
            let message = "source path is not a regular file";
            return Err(if is_import {
                Diagnostic::import(Some(logical_name.clone()), message)
            } else {
                Diagnostic::io(
                    Some(logical_name.clone()),
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, message),
                )
            });
        }

        let expected = SourceNodeSnapshot::from_metadata(&metadata);
        if metadata.len() > max_bytes as u64 {
            return Err(Diagnostic::limit(
                limit_kind,
                Some(logical_name),
                format!("source exceeds the {max_bytes}-byte limit"),
            ));
        }

        const MAX_INTERRUPTED_READ_RETRIES: usize = 1_024;
        let limit = max_bytes.saturating_add(1);
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        let mut buffer = [0_u8; 8 * 1024];
        let mut interruptions = 0usize;
        while bytes.len() < limit {
            let remaining = limit - bytes.len();
            let chunk = remaining.min(buffer.len());
            match file.read(&mut buffer[..chunk]) {
                Ok(0) => break,
                Ok(read) => bytes.extend_from_slice(&buffer[..read]),
                Err(error)
                    if error.kind() == std::io::ErrorKind::Interrupted
                        && interruptions < MAX_INTERRUPTED_READ_RETRIES =>
                {
                    interruptions += 1;
                }
                Err(error) => return Err(Diagnostic::io(Some(logical_name.clone()), error)),
            }
        }
        if bytes.len() > max_bytes {
            return Err(Diagnostic::limit(
                limit_kind,
                Some(logical_name),
                format!("source exceeds the {max_bytes}-byte limit"),
            ));
        }
        let final_metadata = file
            .metadata()
            .map_err(|error| Diagnostic::io(Some(logical_name.clone()), error))?;
        let named = openat2_file(
            self.directory.as_raw_fd(),
            relative.as_os_str().as_bytes(),
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            resolve,
            &self.canonical.join(&relative),
        )
        .map_err(|error| load_error(&logical_name, error, is_import))?;
        let named_metadata = named
            .metadata()
            .map_err(|error| Diagnostic::io(Some(logical_name.clone()), error))?;
        if SourceNodeSnapshot::from_metadata(&final_metadata) != expected
            || SourceNodeSnapshot::from_metadata(&named_metadata) != expected
        {
            return Err(Diagnostic::io(
                Some(logical_name.clone()),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "source file changed while it was being read",
                ),
            ));
        }
        self.verify_retained_directories()?;
        let text = String::from_utf8(bytes).map_err(|error| {
            Diagnostic::io(
                Some(logical_name.clone()),
                std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            )
        })?;
        Ok(Source::new(logical_name, text))
    }

    fn retain_intermediate_directories(&self, relative: &Path) -> std::io::Result<()> {
        let Some(retained) = &self.retained_directories else {
            return Ok(());
        };
        let mut retained = retained
            .lock()
            .map_err(|_| std::io::Error::other("retained source-directory witnesses were poisoned"))?;
        let mut prefix = PathBuf::new();
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        for component in parent.components() {
            prefix.push(component.as_os_str());
            self.verify_retained_directories_locked(&retained)
                .map_err(|(_, error)| error)?;
            if retained.contains_key(&prefix) {
                continue;
            }

            let descriptor = openat2_file(
                self.directory.as_raw_fd(),
                prefix.as_os_str().as_bytes(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV,
                &self.canonical.join(&prefix),
            )?;
            let metadata = descriptor.metadata()?;
            if !metadata.file_type().is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    "source path intermediate component is not a directory",
                ));
            }
            let expected = SourceNodeSnapshot::from_metadata(&metadata);
            self.verify_retained_directories_locked(&retained)
                .map_err(|(_, error)| error)?;
            retained.insert(prefix.clone(), RetainedDirectory { descriptor, expected });
        }
        self.verify_retained_directories_locked(&retained)
            .map_err(|(_, error)| error)
    }

    fn verify_retained_directories_locked(
        &self,
        retained: &BTreeMap<PathBuf, RetainedDirectory>,
    ) -> Result<(), (PathBuf, std::io::Error)> {
        if let Some(expected) = self.retained_root {
            let metadata = self.directory.metadata().map_err(|error| (PathBuf::from("."), error))?;
            if !metadata.file_type().is_dir() || SourceNodeSnapshot::from_metadata(&metadata) != expected {
                return Err((
                    PathBuf::from("."),
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "source root changed while configuration was being evaluated",
                    ),
                ));
            }
        }
        for (relative, witness) in retained {
            let descriptor_metadata = witness
                .descriptor
                .metadata()
                .map_err(|error| (relative.clone(), error))?;
            let named = openat2_file(
                self.directory.as_raw_fd(),
                relative.as_os_str().as_bytes(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV,
                &self.canonical.join(relative),
            )
            .map_err(|error| (relative.clone(), error))?;
            let named_metadata = named.metadata().map_err(|error| (relative.clone(), error))?;
            if !descriptor_metadata.file_type().is_dir()
                || !named_metadata.file_type().is_dir()
                || SourceNodeSnapshot::from_metadata(&descriptor_metadata) != witness.expected
                || SourceNodeSnapshot::from_metadata(&named_metadata) != witness.expected
            {
                return Err((
                    relative.clone(),
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "source directory changed while configuration was being evaluated",
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn load_error(logical_name: &str, error: std::io::Error, is_import: bool) -> Diagnostic {
    let error = if error.raw_os_error() == Some(libc::ELOOP) {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "source paths cannot contain symbolic links",
        )
    } else {
        error
    };
    if is_import {
        Diagnostic::import(
            Some(logical_name.to_owned()),
            format!("configuration import cannot be loaded: {error}"),
        )
    } else {
        Diagnostic::io(Some(logical_name.to_owned()), error)
    }
}

fn openat2_file(dirfd: RawFd, path: &[u8], flags: i32, resolve: u64, diagnostic_path: &Path) -> std::io::Result<File> {
    let path = CString::new(path)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path contains a NUL byte"))?;
    // SAFETY: every field in Linux's open_how accepts zero, after which all
    // fields understood by this ABI version are initialized explicitly.
    let mut how: libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = 0;
    how.resolve = resolve;
    const MAX_INTERRUPTED_OPEN_RETRIES: usize = 1_024;
    let mut interruptions = 0;
    let result = loop {
        // SAFETY: path is NUL-terminated, how points to an initialized
        // open_how, and a successful openat2 call returns a new descriptor
        // owned by us.
        let result = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dirfd,
                path.as_ptr(),
                &how,
                size_of::<libc::open_how>(),
            )
        };
        if result != -1 {
            break result;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
        if interruptions == MAX_INTERRUPTED_OPEN_RETRIES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                format!("source open exceeded {MAX_INTERRUPTED_OPEN_RETRIES} interrupted retries"),
            ));
        }
        interruptions += 1;
    };
    // SAFETY: a successful openat2 call returned a fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(result as RawFd) };
    Ok(File::from_parts(descriptor.into(), diagnostic_path))
}

fn normalize_relative(path: &Path, is_import: bool) -> Result<PathBuf, Diagnostic> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                let source_name = Some(path.display().to_string());
                let message = "source path must be relative and cannot contain parent traversal";
                return Err(if is_import {
                    Diagnostic::import(source_name, message)
                } else {
                    Diagnostic::io(
                        source_name,
                        std::io::Error::new(std::io::ErrorKind::PermissionDenied, message),
                    )
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        let source_name = Some(path.display().to_string());
        return Err(if is_import {
            Diagnostic::import(source_name, "source path is empty")
        } else {
            Diagnostic::io(
                source_name,
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path is empty"),
            )
        });
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use fs_err as fs;

    use super::SourceRoot;

    #[test]
    fn replacement_between_root_open_and_canonicalization_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let configured = directory.path().join("root");
        let displaced = directory.path().join("displaced");
        fs::create_dir(&configured).unwrap();

        let error = SourceRoot::new_with_hook(&configured, || {
            fs::rename(&configured, &displaced).unwrap();
            fs::create_dir(&configured).unwrap();
        })
        .unwrap_err();

        assert_eq!(
            error.source_name.as_deref(),
            Some(configured.to_string_lossy().as_ref())
        );
        assert!(error.message.contains("changed while it was being opened"));
    }

    #[test]
    fn descriptor_root_rejects_substitution_beneath_a_retained_import_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let collection = root.join("rooted.d");
        let modules = collection.join("modules");
        let nested = modules.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(collection.join("main.decl"), "root declaration").unwrap();
        fs::write(modules.join("anchor.decl"), "anchor").unwrap();
        fs::write(nested.join("value.decl"), "retained").unwrap();
        let retained = fs::File::open(&root).unwrap();
        let source_root = SourceRoot::from_directory(&root, &retained).unwrap();

        source_root.load("rooted.d/main.decl", 1_024).unwrap();
        source_root
            .load_import(std::path::Path::new("rooted.d/modules/anchor.decl"), 1_024)
            .unwrap();

        fs::rename(&nested, temporary.path().join("detached-nested")).unwrap();
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("value.decl"), "injected").unwrap();

        let error = source_root
            .load_import(std::path::Path::new("rooted.d/modules/nested/value.decl"), 1_024)
            .unwrap_err();
        assert!(error.message.contains("source directory changed"));
    }
}
