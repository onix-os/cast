//! Retained namespace authority for writable system-client startup.

use std::{
    io,
    os::fd::AsRawFd as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::linux_fs::authenticated_procfs_descriptor_child_path;

use super::{
    CAST_DIRECTORY_NAME, ControlledDirectory, DatabaseKind, Error, LOCKFILE_NAME, controlled_resolution, lockfile,
    lockfile_identity, openat2_file, require_controlled_directory, require_controlled_lockfile,
    require_lockfile_identity, require_named_controlled_child, require_named_installation_root, require_no_default_acl,
    require_same_directory,
};

/// Exact fixed directories retained while provisioning a writable installation.
#[derive(Debug)]
pub(super) struct ProvisionedDirectories {
    pub(super) cast: ControlledDirectory,
    pub(super) database: ControlledDirectory,
}

/// Exact `.cast`, database-directory, and installation-lock authority shared by
/// every clone of a writable Installation.
#[derive(Debug)]
pub(super) struct Authority {
    cast: ControlledDirectory,
    database: ControlledDirectory,
    global_lock: lockfile::Lock,
}

/// A SQLite path rooted in one retained database-directory descriptor.
///
/// The descriptor must remain alive for the complete SQLite connection
/// lifetime because the URL addresses it through `/proc/self/fd`.
#[derive(Debug)]
pub(crate) struct DatabaseLocation {
    url: String,
    directory_anchor: Arc<std::fs::File>,
    path: PathBuf,
    kind: DatabaseKind,
}

impl Authority {
    pub(super) fn new(directories: ProvisionedDirectories, global_lock: lockfile::Lock) -> Self {
        Self {
            cast: directories.cast,
            database: directories.database,
            global_lock,
        }
    }

    /// Revalidate the complete parent-child chain and the exact lock inode at
    /// both ends of one proof. A retained descriptor alone is safe to mutate,
    /// but this proof is what establishes that its results remain reachable
    /// through the public installation namespace.
    pub(super) fn revalidate(&self, root_path: &Path, root: &std::fs::File) -> Result<(), Error> {
        require_named_installation_root(root_path, root).map_err(|source| Error::ValidateRootDirectory {
            path: root_path.to_owned(),
            source,
        })?;
        self.revalidate_cast(root)?;
        self.revalidate_global_lock()?;
        self.revalidate_database()?;
        self.revalidate_database()?;
        self.revalidate_global_lock()?;
        self.revalidate_cast(root)?;
        require_named_installation_root(root_path, root).map_err(|source| Error::ValidateRootDirectory {
            path: root_path.to_owned(),
            source,
        })
    }

    pub(super) fn database_location(&self, kind: DatabaseKind) -> Result<DatabaseLocation, Error> {
        self.revalidate_database()?;
        let directory_anchor =
            self.database
                .file
                .try_clone()
                .map(Arc::new)
                .map_err(|source| Error::PrepareDirectory {
                    path: self.database.path.clone(),
                    source,
                })?;
        require_controlled_directory(&directory_anchor, &self.database.path)
            .and_then(|()| require_no_default_acl(&directory_anchor, &self.database.path))
            .and_then(|()| require_same_directory(&self.database.file, &directory_anchor, &self.database.path))
            .map_err(|source| Error::PrepareDirectory {
                path: self.database.path.clone(),
                source,
            })?;

        let url = authenticated_database_url(&directory_anchor, &self.database.path, kind)?;
        Ok(DatabaseLocation {
            url,
            directory_anchor,
            path: self.database.path.clone(),
            kind,
        })
    }

    pub(super) fn cast_directory(&self) -> &std::fs::File {
        &self.cast.file
    }

    fn revalidate_cast(&self, root: &std::fs::File) -> Result<(), Error> {
        require_named_controlled_child(root, CAST_DIRECTORY_NAME, &self.cast).map_err(|source| {
            Error::PrepareDirectory {
                path: self.cast.path.clone(),
                source,
            }
        })
    }

    fn revalidate_database(&self) -> Result<(), Error> {
        require_named_controlled_child(&self.cast.file, c"db", &self.database).map_err(|source| {
            Error::PrepareDirectory {
                path: self.database.path.clone(),
                source,
            }
        })
    }

    fn revalidate_global_lock(&self) -> Result<(), Error> {
        let path = self.cast.path.join(LOCKFILE_NAME.to_string_lossy().as_ref());
        revalidate_lockfile(&self.cast.file, self.global_lock.file(), &path)
            .map_err(|source| Error::PrepareLockfile { path, source })
    }
}

impl DatabaseLocation {
    pub(crate) fn parts(&self) -> (&str, Arc<std::fs::File>) {
        (&self.url, Arc::clone(&self.directory_anchor))
    }

    /// Repeat the procfs and exact descriptor-alias proof after SQLite has
    /// opened this URL. The surrounding construction stage separately repeats
    /// the public installation namespace proof.
    pub(crate) fn revalidate(&self) -> Result<(), Error> {
        let current = authenticated_database_url(&self.directory_anchor, &self.path, self.kind)?;
        if current == self.url {
            Ok(())
        } else {
            Err(Error::PrepareDirectory {
                path: self.path.clone(),
                source: io::Error::other("authenticated SQLite descriptor alias changed"),
            })
        }
    }
}

fn authenticated_database_url(directory: &std::fs::File, path: &Path, kind: DatabaseKind) -> Result<String, Error> {
    require_controlled_directory(directory, path)
        .and_then(|()| require_no_default_acl(directory, path))
        .map_err(|source| Error::PrepareDirectory {
            path: path.to_owned(),
            source,
        })?;
    authenticated_procfs_descriptor_child_path(directory, kind.name()).map_err(|source| Error::PrepareDirectory {
        path: path.to_owned(),
        source,
    })
}

fn revalidate_lockfile(directory: &std::fs::File, retained: &std::fs::File, path: &Path) -> io::Result<()> {
    require_controlled_lockfile(retained, path)?;
    let expected = lockfile_identity(retained)?;
    let named = openat2_file(
        directory.as_raw_fd(),
        LOCKFILE_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_controlled_lockfile(&named, path)?;
    require_lockfile_identity(expected, lockfile_identity(&named)?)
}
