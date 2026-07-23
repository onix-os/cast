use super::*;

impl PreparedFile {
    pub(super) fn new(directory: &RetainedDirectory, bytes: &[u8], path: PathBuf) -> Result<Self, MetadataError> {
        directory.require_retained()?;
        let file = openat2_file(
            directory.file.as_raw_fd(),
            c".",
            nix::libc::O_TMPFILE
                | nix::libc::O_RDWR
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            TEMPORARY_FILE_MODE,
            controlled_resolution(),
        )
        .map_err(|source| metadata_io("create anonymous candidate metadata", &path, source))?;
        file.set_permissions(Permissions::from_mode(TEMPORARY_FILE_MODE))
            .map_err(|source| metadata_io("normalize anonymous metadata mode", &path, source))?;
        require_anonymous(&file, &path, TEMPORARY_FILE_MODE, 0)?;
        write_all_at(&file, bytes, &path)?;
        file.sync_all()
            .map_err(|source| metadata_io("sync anonymous metadata contents", &path, source))?;
        file.set_permissions(Permissions::from_mode(CANONICAL_FILE_MODE))
            .map_err(|source| metadata_io("seal anonymous metadata mode", &path, source))?;
        file.sync_all()
            .map_err(|source| metadata_io("sync sealed anonymous metadata", &path, source))?;
        let witness = require_anonymous(&file, &path, CANONICAL_FILE_MODE, bytes.len())?;
        if read_exact_at(&file, bytes.len(), &path, "read back anonymous metadata")? != bytes
            || require_anonymous(&file, &path, CANONICAL_FILE_MODE, bytes.len())? != witness
        {
            return Err(MetadataError::FileChanged { path });
        }
        require_no_access_acl(&file, &path)
            .map_err(|source| metadata_io("reject access ACL on anonymous metadata", &path, source))?;
        directory.require_retained()?;
        Ok(Self {
            file,
            identity: (witness.device, witness.inode),
        })
    }
}

impl RetainedDirectory {
    pub(super) fn retain_or_create(parent: &File, name: &CStr, path: PathBuf) -> Result<Self, MetadataError> {
        let probe = match openat2_file(
            parent.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        ) {
            Ok(file) => Some(file),
            Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => None,
            Err(source) => return Err(metadata_io("probe candidate metadata directory", &path, source)),
        };
        if let Some(probe) = probe {
            return Self::open_existing(parent, name, path, probe);
        }
        Self::create_private_and_publish(parent, name, path)
    }

    fn open_existing(parent: &File, name: &CStr, path: PathBuf, probe: File) -> Result<Self, MetadataError> {
        let expected = directory_witness(&probe, &path)?;
        let retained = Self::open(parent, name, path)?;
        if retained.witness != expected {
            return Err(MetadataError::DirectoryChanged { path: retained.path });
        }
        Ok(retained)
    }

    pub(super) fn open(parent: &File, name: &CStr, path: PathBuf) -> Result<Self, MetadataError> {
        let file = openat2_file(
            parent.as_raw_fd(),
            name,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            0,
            controlled_resolution(),
        )
        .map_err(|source| metadata_io("open retained metadata directory", &path, source))?;
        let witness = directory_witness(&file, &path)?;
        require_no_access_acl(&file, &path)
            .map_err(|source| metadata_io("reject access ACL on metadata directory", &path, source))?;
        require_no_default_acl(&file, &path)
            .map_err(|source| metadata_io("reject default ACL on metadata directory", &path, source))?;
        Ok(Self { file, path, witness })
    }

    pub(super) fn require_named(&self, parent: &File, name: &CStr) -> Result<(), MetadataError> {
        self.require_retained()?;
        let named = openat2_file(
            parent.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        )
        .map_err(|source| metadata_io("revalidate metadata directory name", &self.path, source))?;
        if directory_witness(&named, &self.path)? == self.witness {
            Ok(())
        } else {
            Err(MetadataError::DirectoryChanged {
                path: self.path.clone(),
            })
        }
    }

    pub(super) fn require_retained(&self) -> Result<(), MetadataError> {
        if directory_witness(&self.file, &self.path)? != self.witness {
            return Err(MetadataError::DirectoryChanged {
                path: self.path.clone(),
            });
        }
        require_no_access_acl(&self.file, &self.path)
            .map_err(|source| metadata_io("revalidate metadata directory access ACL", &self.path, source))?;
        require_no_default_acl(&self.file, &self.path)
            .map_err(|source| metadata_io("revalidate metadata directory default ACL", &self.path, source))
    }

    pub(super) fn require_absent(&self, name: &CStr) -> Result<(), MetadataError> {
        self.require_retained()?;
        let path = self.path.join(name.to_string_lossy().as_ref());
        match openat2_file(
            self.file.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        ) {
            Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(()),
            Err(source) => Err(metadata_io("probe candidate metadata destination", path, source)),
            Ok(file) => {
                let metadata = file
                    .metadata()
                    .map_err(|source| metadata_io("inspect candidate metadata destination", &path, source))?;
                Err(MetadataError::DestinationExists {
                    path,
                    kind: file_type_name(&metadata.file_type()),
                    owner: metadata.uid(),
                    mode: metadata.permissions().mode() & 0o7777,
                    links: metadata.nlink(),
                })
            }
        }
    }

    pub(super) fn require_empty(&self) -> Result<(), MetadataError> {
        self.require_retained()?;
        private_directory::require_empty_directory(&self.file, &self.path)?;
        self.require_retained()
    }

    pub(super) fn sync(&self) -> Result<(), MetadataError> {
        self.require_retained()?;
        self.file
            .sync_all()
            .map_err(|source| metadata_io("sync retained metadata directory", &self.path, source))?;
        self.require_retained()
    }
}

impl FileWitness {
    pub(super) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            mode: metadata.permissions().mode() & 0o7777,
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

pub(super) fn directory_witness(file: &File, path: &Path) -> Result<DirectoryWitness, MetadataError> {
    let metadata = file
        .metadata()
        .map_err(|source| metadata_io("inspect candidate metadata directory", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_user_id()
        || mode & 0o7000 != 0
        || mode & 0o700 != 0o700
        || mode & 0o022 != 0
    {
        return Err(MetadataError::UnsafeDirectory {
            path: path.to_owned(),
            kind: file_type_name(&metadata.file_type()),
            owner: metadata.uid(),
            mode,
        });
    }
    Ok(DirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode,
    })
}

fn require_anonymous(file: &File, path: &Path, mode: u32, length: usize) -> Result<FileWitness, MetadataError> {
    let metadata = file
        .metadata()
        .map_err(|source| metadata_io("inspect anonymous candidate metadata", path, source))?;
    let witness = FileWitness::from_metadata(&metadata);
    if metadata.file_type().is_file()
        && witness.owner == effective_user_id()
        && witness.mode == mode
        && witness.links == 0
        && witness.length == length as u64
    {
        Ok(witness)
    } else {
        Err(MetadataError::FileChanged { path: path.to_owned() })
    }
}

pub(super) fn published_witness(file: &File, path: &Path, length: usize) -> Result<FileWitness, MetadataError> {
    let metadata = file
        .metadata()
        .map_err(|source| metadata_io("inspect published candidate metadata", path, source))?;
    let witness = FileWitness::from_metadata(&metadata);
    if metadata.file_type().is_file()
        && witness.owner == effective_user_id()
        && witness.mode == CANONICAL_FILE_MODE
        && witness.links == 1
        && witness.length == length as u64
    {
        Ok(witness)
    } else {
        Err(MetadataError::FileChanged { path: path.to_owned() })
    }
}

fn write_all_at(file: &File, bytes: &[u8], path: &Path) -> Result<(), MetadataError> {
    let mut written = 0usize;
    let mut attempts = 0usize;
    while written < bytes.len() {
        attempts += 1;
        if attempts > MAX_IO_ATTEMPTS {
            return Err(metadata_io(
                "write complete anonymous candidate metadata",
                path,
                io::Error::other("metadata write exceeded the bounded attempt limit"),
            ));
        }
        match file.write_at(&bytes[written..], written as u64) {
            Ok(0) => {
                return Err(metadata_io(
                    "write anonymous candidate metadata",
                    path,
                    io::Error::from_raw_os_error(nix::libc::EIO),
                ));
            }
            Ok(count) => written += count,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(metadata_io("write anonymous candidate metadata", path, source)),
        }
    }
    Ok(())
}

pub(super) fn read_exact_at(
    file: &File,
    length: usize,
    path: &Path,
    operation: &'static str,
) -> Result<Vec<u8>, MetadataError> {
    let mut bytes = vec![0; length];
    let mut read = 0usize;
    let mut attempts = 0usize;
    while read < bytes.len() {
        attempts += 1;
        if attempts > MAX_IO_ATTEMPTS {
            return Err(metadata_io(
                operation,
                path,
                io::Error::other("metadata read exceeded the bounded attempt limit"),
            ));
        }
        match file.read_at(&mut bytes[read..], read as u64) {
            Ok(0) => {
                return Err(metadata_io(
                    operation,
                    path,
                    io::Error::from(io::ErrorKind::UnexpectedEof),
                ));
            }
            Ok(count) => read += count,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(metadata_io(operation, path, source)),
        }
    }
    Ok(bytes)
}

pub(super) fn file_type_name(file_type: &std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt as _;

    if file_type.is_file() {
        "regular-file"
    } else if file_type.is_dir() {
        "directory"
    } else if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_fifo() {
        "fifo"
    } else if file_type.is_char_device() {
        "character-device"
    } else if file_type.is_block_device() {
        "block-device"
    } else if file_type.is_socket() {
        "socket"
    } else {
        "unknown"
    }
}

pub(super) fn metadata_io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> MetadataError {
    MetadataError::Io {
        operation,
        path: path.into(),
        source,
    }
}

pub(super) fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { nix::libc::geteuid() }
}
