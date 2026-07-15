use super::{artifact_verification::*, *};

#[derive(Debug)]
pub(super) struct DirectoryHandle {
    pub(super) path: PathBuf,
    pub(super) file: File,
    pub(super) identity: Identity,
}

impl DirectoryHandle {
    pub(super) fn open_root(path: &Path) -> Result<Self, ArtifactError> {
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

    pub(super) fn open_child_directory(&self, name: &[u8]) -> Result<Self, ArtifactError> {
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

    pub(super) fn display(&self, name: &[u8]) -> PathBuf {
        self.path.join(OsStr::from_bytes(name))
    }

    pub(super) fn metadata(&self, operation: &'static str) -> Result<Metadata, ArtifactError> {
        self.file.metadata().map_err(|source| ArtifactError::Io {
            operation,
            path: self.path.clone(),
            source,
        })
    }

    pub(super) fn require_path_identity(&self) -> Result<(), ArtifactError> {
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

    pub(super) fn inspect(
        &self,
        name: &[u8],
        operation: &'static str,
    ) -> Result<Option<(Metadata, Identity)>, ArtifactError> {
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

    pub(super) fn require_inventory(&self, role: &'static str, expected: &[Vec<u8>]) -> Result<(), ArtifactError> {
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
