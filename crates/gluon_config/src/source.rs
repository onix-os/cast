use std::{
    ffi::CString,
    fmt,
    io::Read,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceRootIdentity {
    device: u64,
    inode: u64,
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
        })
    }

    pub fn path(&self) -> &Path {
        &self.canonical
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
        let mut file = openat2_file(
            self.directory.as_raw_fd(),
            relative.as_os_str().as_bytes(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
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

        let mut bytes = Vec::new();
        file.by_ref()
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| Diagnostic::io(Some(logical_name.clone()), error))?;
        if bytes.len() > max_bytes {
            return Err(Diagnostic::limit(
                limit_kind,
                Some(logical_name),
                format!("source exceeds the {max_bytes}-byte limit"),
            ));
        }
        let text = String::from_utf8(bytes).map_err(|error| {
            Diagnostic::io(
                Some(logical_name.clone()),
                std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            )
        })?;
        Ok(Source::new(logical_name, text))
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
    // SAFETY: path is NUL-terminated, how points to an initialized open_how,
    // and a successful openat2 call returns a new descriptor owned by us.
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
        return Err(std::io::Error::last_os_error());
    }
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
}
