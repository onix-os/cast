//! Existing-only authority for explicit read-only installation snapshots.

use std::{io, os::fd::AsRawFd as _, path::Path, time::Duration};

use super::{
    CAST_DIRECTORY_NAME, ControlledDirectory, Error, LOCKFILE_NAME, controlled_resolution, lockfile, lockfile_identity,
    open_controlled_directory_path, openat2_file, require_controlled_directory, require_controlled_lockfile,
    require_lockfile_identity, require_named_controlled_child, require_named_controlled_directory_path,
    require_named_installation_root, require_no_default_acl, require_same_directory,
};

pub(super) const SHARED_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const CACHE_DIRECTORY_NAME: &std::ffi::CStr = c"cache";

/// Retained namespace and shared-lock authority for one explicit read-only
/// installation snapshot.
#[derive(Debug)]
pub(super) struct Authority {
    cast: ControlledDirectory,
    global_lock: RetainedSharedLock,
    cache: CacheAuthority,
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
    path: std::path::PathBuf,
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
        path: std::path::PathBuf,
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

#[cfg(test)]
mod tests;
