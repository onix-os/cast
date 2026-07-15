//! Existing-only authority for explicit read-only installation snapshots.

use std::{
    ffi::CString,
    io,
    mem::MaybeUninit,
    os::{
        fd::AsRawFd as _,
        unix::fs::{FileExt as _, MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use super::{
    CAST_DIRECTORY_NAME, ControlledDirectory, Error, LOCKFILE_NAME, controlled_resolution, lockfile, lockfile_identity,
    open_controlled_directory_path, openat2_file, require_controlled_directory, require_controlled_lockfile,
    require_lockfile_identity, require_named_controlled_child, require_named_controlled_directory_path,
    require_named_installation_root, require_no_default_acl, require_same_directory,
};
use nix::unistd::Uid;

pub(super) const SHARED_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const CACHE_DIRECTORY_NAME: &std::ffi::CStr = c"cache";
const DATABASE_DIRECTORY_NAME: &std::ffi::CStr = c"db";

/// Retained namespace and shared-lock authority for one explicit read-only
/// installation snapshot.
#[derive(Debug)]
pub(super) struct Authority {
    cast: ControlledDirectory,
    database: ControlledDirectory,
    global_lock: RetainedSharedLock,
    cache: CacheAuthority,
}

/// One exact existing database inode retained for a read-only SQLite handle.
#[derive(Clone, Debug)]
#[allow(dead_code)] // retained immutable image authority for the next client slice
pub(crate) struct DatabaseFile {
    directory: Arc<std::fs::File>,
    file: Arc<std::fs::File>,
    identity: (u64, u64),
    path: PathBuf,
    kind: DatabaseKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // variants are selected by the next read-only-client slice
pub(crate) enum DatabaseKind {
    Install,
    State,
    Layout,
}

impl DatabaseKind {
    fn name(self) -> &'static std::ffi::CStr {
        match self {
            Self::Install => c"install",
            Self::State => c"state",
            Self::Layout => c"layout",
        }
    }
}

#[derive(Debug)]
enum CacheAuthority {
    Default(ControlledDirectory),
    Custom(CustomCacheAuthority),
}

#[derive(Debug)]
enum UnlockedCacheAuthority {
    Default(ControlledDirectory),
    Custom(ControlledDirectory),
}

#[derive(Debug)]
struct CustomCacheAuthority {
    directory: ControlledDirectory,
    lock: RetainedSharedLock,
}

#[derive(Debug)]
struct RetainedSharedLock {
    lock: lockfile::Lock,
    identity: (u64, u64),
    path: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DatabaseWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
    accessed_seconds: i64,
    accessed_nanoseconds: i64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl Authority {
    pub(super) fn open(
        root_path: &Path,
        root: &std::fs::File,
        custom_cache_path: Option<&Path>,
        lock_timeout: Duration,
    ) -> Result<Self, Error> {
        require_named_installation_root(root_path, root).map_err(|source| Error::ValidateRootDirectory {
            path: root_path.to_owned(),
            source,
        })?;

        let cast_path = root_path.join(CAST_DIRECTORY_NAME.to_string_lossy().as_ref());
        let cast = open_existing_controlled_child(root, CAST_DIRECTORY_NAME, &cast_path).map_err(|source| {
            Error::OpenReadOnlySnapshotDirectory {
                path: cast_path.clone(),
                source,
            }
        })?;
        require_named_controlled_child(root, CAST_DIRECTORY_NAME, &cast).map_err(|source| {
            Error::OpenReadOnlySnapshotDirectory {
                path: cast.path.clone(),
                source,
            }
        })?;

        let database_path = cast.path.join(DATABASE_DIRECTORY_NAME.to_string_lossy().as_ref());
        let database =
            open_existing_controlled_child(&cast.file, DATABASE_DIRECTORY_NAME, &database_path).map_err(|source| {
                Error::OpenReadOnlySnapshotDirectory {
                    path: database_path,
                    source,
                }
            })?;

        let cache = match custom_cache_path {
            Some(path) => UnlockedCacheAuthority::Custom(
                open_controlled_directory_path(path)
                    .map(|file| ControlledDirectory {
                        file,
                        path: path.to_owned(),
                    })
                    .map_err(|source| Error::OpenReadOnlySnapshotDirectory {
                        path: path.to_owned(),
                        source,
                    })?,
            ),
            None => {
                let path = cast.path.join(CACHE_DIRECTORY_NAME.to_string_lossy().as_ref());
                UnlockedCacheAuthority::Default(
                    open_existing_controlled_child(&cast.file, CACHE_DIRECTORY_NAME, &path)
                        .map_err(|source| Error::OpenReadOnlySnapshotDirectory { path, source })?,
                )
            }
        };

        require_named_installation_root(root_path, root).map_err(|source| Error::ValidateRootDirectory {
            path: root_path.to_owned(),
            source,
        })?;
        require_named_controlled_child(root, CAST_DIRECTORY_NAME, &cast).map_err(|source| {
            Error::OpenReadOnlySnapshotDirectory {
                path: cast.path.clone(),
                source,
            }
        })?;
        require_named_controlled_child(&cast.file, DATABASE_DIRECTORY_NAME, &database).map_err(|source| {
            Error::OpenReadOnlySnapshotDirectory {
                path: database.path.clone(),
                source,
            }
        })?;
        cache.revalidate_before_lock(&cast)?;

        let global_lock = RetainedSharedLock::open(
            &cast.file,
            cast.path.join(LOCKFILE_NAME.to_string_lossy().as_ref()),
            "Blocking: another process is mutating the Cast root",
            lock_timeout,
        )?;
        let cache = match cache {
            UnlockedCacheAuthority::Default(directory) => CacheAuthority::Default(directory),
            UnlockedCacheAuthority::Custom(directory) => {
                let lock = RetainedSharedLock::open(
                    &directory.file,
                    directory.path.join(LOCKFILE_NAME.to_string_lossy().as_ref()),
                    "Blocking: another process is mutating the cache dir",
                    lock_timeout,
                )?;
                CacheAuthority::Custom(CustomCacheAuthority { directory, lock })
            }
        };

        let authority = Self {
            cast,
            database,
            global_lock,
            cache,
        };
        authority.revalidate(root_path, root)?;
        Ok(authority)
    }

    pub(super) fn revalidate(&self, root_path: &Path, root: &std::fs::File) -> Result<(), Error> {
        require_named_installation_root(root_path, root).map_err(|source| Error::ValidateRootDirectory {
            path: root_path.to_owned(),
            source,
        })?;
        require_named_controlled_child(root, CAST_DIRECTORY_NAME, &self.cast).map_err(|source| {
            Error::OpenReadOnlySnapshotDirectory {
                path: self.cast.path.clone(),
                source,
            }
        })?;
        self.global_lock.revalidate(&self.cast.file)?;
        require_named_controlled_child(&self.cast.file, DATABASE_DIRECTORY_NAME, &self.database).map_err(|source| {
            Error::OpenReadOnlySnapshotDirectory {
                path: self.database.path.clone(),
                source,
            }
        })?;
        self.cache.revalidate(&self.cast)?;

        require_named_controlled_child(root, CAST_DIRECTORY_NAME, &self.cast).map_err(|source| {
            Error::OpenReadOnlySnapshotDirectory {
                path: self.cast.path.clone(),
                source,
            }
        })?;
        require_named_installation_root(root_path, root).map_err(|source| Error::ValidateRootDirectory {
            path: root_path.to_owned(),
            source,
        })
    }

    #[allow(dead_code)] // consumed through Installation by the next client slice
    pub(super) fn open_database(&self, kind: DatabaseKind) -> Result<DatabaseFile, Error> {
        self.revalidate_database_directory()?;
        let path = self.database.path.join(kind.name().to_string_lossy().as_ref());
        let file = openat2_file(
            self.database.file.as_raw_fd(),
            kind.name(),
            nix::libc::O_RDONLY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK
                | nix::libc::O_NOATIME,
            0,
            controlled_resolution(),
        )
        .and_then(|file| {
            require_read_only_database_file(&file, &path)?;
            Ok(file)
        })
        .map_err(|source| Error::OpenReadOnlyDatabase {
            path: path.clone(),
            source,
        })?;
        let identity = database_identity(&file).map_err(|source| Error::OpenReadOnlyDatabase {
            path: path.clone(),
            source,
        })?;
        let directory = self
            .database
            .file
            .try_clone()
            .map_err(|source| Error::OpenReadOnlyDatabase {
                path: self.database.path.clone(),
                source,
            })?;
        let database = DatabaseFile {
            directory: Arc::new(directory),
            file: Arc::new(file),
            identity,
            path,
            kind,
        };
        self.revalidate_database(&database)?;
        Ok(database)
    }

    #[allow(dead_code)] // consumed through Installation by the next client slice
    pub(super) fn read_database_image(&self, database: &DatabaseFile, max_bytes: usize) -> Result<Box<[u8]>, Error> {
        self.revalidate_database(database)?;
        let initial = database_witness(&database.file).map_err(|source| Error::ReadOnlyDatabaseImage {
            path: database.path.clone(),
            source,
        })?;
        let length = usize::try_from(initial.length).map_err(|_| Error::ReadOnlyDatabaseTooLarge {
            path: database.path.clone(),
            size: initial.length,
            limit: max_bytes,
        })?;
        if length > max_bytes {
            return Err(Error::ReadOnlyDatabaseTooLarge {
                path: database.path.clone(),
                size: initial.length,
                limit: max_bytes,
            });
        }

        let mut image = vec![0_u8; length];
        let mut offset = 0usize;
        // EINTR retries are bounded by the admitted image length and run while
        // the retained global cooperating-writer flock is held. This is not a
        // deadline against hostile kernel scheduling or raw inode writers.
        while offset < image.len() {
            match database.file.read_at(&mut image[offset..], offset as u64) {
                Ok(0) => {
                    return Err(Error::ReadOnlyDatabaseImageChanged {
                        path: database.path.clone(),
                    });
                }
                Ok(read) => offset += read,
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                Err(source) => {
                    return Err(Error::ReadOnlyDatabaseImage {
                        path: database.path.clone(),
                        source,
                    });
                }
            }
        }
        let mut trailing = [0_u8; 1];
        loop {
            match database.file.read_at(&mut trailing, initial.length) {
                Ok(0) => break,
                Ok(_) => {
                    return Err(Error::ReadOnlyDatabaseImageChanged {
                        path: database.path.clone(),
                    });
                }
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                Err(source) => {
                    return Err(Error::ReadOnlyDatabaseImage {
                        path: database.path.clone(),
                        source,
                    });
                }
            }
        }
        let final_witness = database_witness(&database.file).map_err(|source| Error::ReadOnlyDatabaseImage {
            path: database.path.clone(),
            source,
        })?;
        if initial != final_witness {
            return Err(Error::ReadOnlyDatabaseImageChanged {
                path: database.path.clone(),
            });
        }
        self.revalidate_database(database)?;
        Ok(image.into_boxed_slice())
    }

    #[allow(dead_code)] // consumed through Installation by the next client slice
    pub(super) fn revalidate_database(&self, database: &DatabaseFile) -> Result<(), Error> {
        self.revalidate_database_directory()?;
        require_same_directory(&self.database.file, &database.directory, &self.database.path).map_err(|source| {
            Error::OpenReadOnlySnapshotDirectory {
                path: self.database.path.clone(),
                source,
            }
        })?;
        require_read_only_database_file(&database.file, &database.path).map_err(|source| {
            Error::OpenReadOnlyDatabase {
                path: database.path.clone(),
                source,
            }
        })?;
        let retained = database_identity(&database.file).map_err(|source| Error::OpenReadOnlyDatabase {
            path: database.path.clone(),
            source,
        })?;
        if retained != database.identity {
            return Err(Error::ReadOnlyDatabaseChanged {
                path: database.path.clone(),
            });
        }
        let named = openat2_file(
            self.database.file.as_raw_fd(),
            database.kind.name(),
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        )
        .and_then(|file| {
            require_read_only_database_file(&file, &database.path)?;
            database_identity(&file)
        })
        .map_err(|source| Error::OpenReadOnlyDatabase {
            path: database.path.clone(),
            source,
        })?;
        if named != database.identity {
            return Err(Error::ReadOnlyDatabaseChanged {
                path: database.path.clone(),
            });
        }
        require_database_sidecars_absent(database)?;
        Ok(())
    }

    fn revalidate_database_directory(&self) -> Result<(), Error> {
        require_named_controlled_child(&self.cast.file, DATABASE_DIRECTORY_NAME, &self.database).map_err(|source| {
            Error::OpenReadOnlySnapshotDirectory {
                path: self.database.path.clone(),
                source,
            }
        })
    }
}

fn require_database_sidecars_absent(database: &DatabaseFile) -> Result<(), Error> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let name = CString::new(format!("{}{suffix}", database.kind.name().to_string_lossy()))
            .expect("fixed SQLite sidecar name contains no NUL");
        let path = database
            .path
            .parent()
            .expect("database path has a retained parent")
            .join(name.to_string_lossy().as_ref());
        let mut metadata = MaybeUninit::<nix::libc::stat>::uninit();
        loop {
            // SAFETY: the retained directory, fixed NUL-terminated name, and
            // writable stat storage remain live for this descriptor-relative
            // metadata lookup. AT_SYMLINK_NOFOLLOW treats any final inode kind
            // as sidecar evidence rather than following it.
            if unsafe {
                nix::libc::fstatat(
                    database.directory.as_raw_fd(),
                    name.as_ptr(),
                    metadata.as_mut_ptr(),
                    nix::libc::AT_SYMLINK_NOFOLLOW,
                )
            } == 0
            {
                return Err(Error::ReadOnlyDatabaseSidecar { path });
            }
            let source = io::Error::last_os_error();
            match source.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::NotFound => break,
                _ => {
                    return Err(Error::OpenReadOnlyDatabase { path, source });
                }
            }
        }
    }
    Ok(())
}

fn database_witness(file: &std::fs::File) -> io::Result<DatabaseWitness> {
    let metadata = file.metadata()?;
    Ok(DatabaseWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.mode() & 0o7777,
        links: metadata.nlink(),
        length: metadata.len(),
        accessed_seconds: metadata.atime(),
        accessed_nanoseconds: metadata.atime_nsec(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

impl CacheAuthority {
    fn revalidate(&self, cast: &ControlledDirectory) -> Result<(), Error> {
        match self {
            Self::Default(directory) => require_named_controlled_child(&cast.file, CACHE_DIRECTORY_NAME, directory)
                .map_err(|source| Error::OpenReadOnlySnapshotDirectory {
                    path: directory.path.clone(),
                    source,
                }),
            Self::Custom(cache) => {
                require_named_controlled_directory_path(&cache.directory.path, &cache.directory.file).map_err(
                    |source| Error::OpenReadOnlySnapshotDirectory {
                        path: cache.directory.path.clone(),
                        source,
                    },
                )?;
                cache.lock.revalidate(&cache.directory.file)
            }
        }
    }
}

impl UnlockedCacheAuthority {
    fn revalidate_before_lock(&self, cast: &ControlledDirectory) -> Result<(), Error> {
        match self {
            Self::Default(directory) => require_named_controlled_child(&cast.file, CACHE_DIRECTORY_NAME, directory)
                .map_err(|source| Error::OpenReadOnlySnapshotDirectory {
                    path: directory.path.clone(),
                    source,
                }),
            Self::Custom(directory) => require_named_controlled_directory_path(&directory.path, &directory.file)
                .map_err(|source| Error::OpenReadOnlySnapshotDirectory {
                    path: directory.path.clone(),
                    source,
                }),
        }
    }
}

impl RetainedSharedLock {
    fn open(
        directory: &std::fs::File,
        path: PathBuf,
        block_message: &str,
        lock_timeout: Duration,
    ) -> Result<Self, Error> {
        let pinned = openat2_file(
            directory.as_raw_fd(),
            LOCKFILE_NAME,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        )
        .and_then(|file| {
            require_controlled_lockfile(&file, &path)?;
            Ok((lockfile_identity(&file)?, file))
        })
        .map_err(|source| Error::OpenReadOnlySnapshotLockfile {
            path: path.clone(),
            source,
        })?;

        let file = openat2_file(
            directory.as_raw_fd(),
            LOCKFILE_NAME,
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
            controlled_resolution(),
        )
        .and_then(|file| {
            require_controlled_lockfile(&file, &path)?;
            require_lockfile_identity(pinned.0, lockfile_identity(&file)?)?;
            Ok(file)
        })
        .map_err(|source| Error::OpenReadOnlySnapshotLockfile {
            path: path.clone(),
            source,
        })?;
        let lock = lockfile::acquire_shared_file(file, block_message, lock_timeout).map_err(|source| match source {
            lockfile::Error::Timeout { timeout } => Error::ReadOnlySnapshotLockTimeout {
                path: path.clone(),
                timeout,
            },
            source => Error::Lockfile(source),
        })?;
        let retained = Self {
            lock,
            identity: pinned.0,
            path,
        };
        retained.revalidate(directory)?;
        Ok(retained)
    }

    fn revalidate(&self, directory: &std::fs::File) -> Result<(), Error> {
        let retained = self.lock.file();
        require_controlled_lockfile(retained, &self.path)
            .and_then(|()| require_lockfile_identity(self.identity, lockfile_identity(retained)?))
            .and_then(|()| {
                let named = openat2_file(
                    directory.as_raw_fd(),
                    LOCKFILE_NAME,
                    nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                    0,
                    controlled_resolution(),
                )?;
                require_controlled_lockfile(&named, &self.path)?;
                require_lockfile_identity(self.identity, lockfile_identity(&named)?)
            })
            .map_err(|source| Error::OpenReadOnlySnapshotLockfile {
                path: self.path.clone(),
                source,
            })
    }
}

fn open_existing_controlled_child(
    parent: &std::fs::File,
    name: &std::ffi::CStr,
    path: &Path,
) -> io::Result<ControlledDirectory> {
    let pinned = openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_controlled_directory(&pinned, path)?;

    let directory = openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_controlled_directory(&directory, path)?;
    require_no_default_acl(&directory, path)?;
    require_same_directory(&pinned, &directory, path)?;

    let retained = ControlledDirectory {
        file: directory,
        path: path.to_owned(),
    };
    require_named_controlled_child(parent, name, &retained)?;
    Ok(retained)
}

fn require_read_only_database_file(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.permissions().mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || mode & 0o7000 != 0
        || mode & 0o022 != 0
        || mode & 0o400 == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "database is not one safe owner-controlled readable file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(())
}

fn database_identity(file: &std::fs::File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(test)]
mod tests;
