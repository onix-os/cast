//! Encapsulation of a target installation filesystem

use std::{
    ffi::{CStr, CString, OsStr},
    fmt,
    io::{self, Read as _, Seek as _, SeekFrom, Write as _},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{FileExt as _, MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use log::{trace, warn};
use nix::unistd::Uid;
use thiserror::Error;
use tui::Styled;

use crate::{
    linux_fs::{
        chmod_path_descriptor, open_path_descriptor_readonly, openat2_file_until, require_no_default_acl,
        require_no_default_acl_until,
    },
    state,
    system_model::{self, LoadedSystemModel},
};

mod lockfile;
mod mutable_namespace;
mod snapshot;
pub(crate) use mutable_namespace::DatabaseLocation as MutableDatabaseLocation;
pub(crate) use snapshot::{DatabaseFile as ReadOnlyDatabaseFile, DatabaseKind};

/// System mutability - do we have readwrite?
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub enum Mutability {
    /// We only have readonly access
    ReadOnly,
    /// We have read-write access
    ReadWrite,
}

/// Encapsulate details for a target installation filesystem
#[derive(Debug, Clone)]
pub struct Installation {
    /// Fully qualified root filesystem path
    pub root: PathBuf,

    /// Filesystem mutability: Will it be mutable or just RO for queries?
    pub mutability: Mutability,

    /// Discovery-time active system state ID.
    ///
    /// This snapshot is deliberately crate-private and is not updated when a
    /// Client completes a transition. It is a stale-detection witness only;
    /// public authority must pass through the descriptor-rooted live gate on
    /// Client operations.
    pub(crate) active_state: Option<state::Id>,

    /// Custom cache directory location,
    /// otherwise derived from root
    pub cache_dir: Option<PathBuf>,

    /// If defined, the user-authored desired system intent selected by a
    /// successfully constructed system Client.
    ///
    /// Opening an Installation never evaluates authored intent. The system
    /// client startup gate populates this only after recovery evidence and the
    /// strict live-state selection have been authenticated.
    pub system_model: Option<LoadedSystemModel>,

    /// Authenticated installation-root capability retained for the lifetime of
    /// every clone. Stateful namespace mutations must be rooted here rather
    /// than reopening `root` as authority.
    root_directory: Arc<std::fs::File>,

    /// Exact writable `.cast`, database-directory, and global-lock authority.
    /// This is absent for naturally read-only and explicit snapshot opens.
    mutable_namespace: Option<Arc<mutable_namespace::Authority>>,

    /// Acquired locks that guarantee exclusive access
    /// to the installation for mutable operations
    _locks: Vec<lockfile::Lock>,

    /// Retained read-only namespace capabilities and shared locks. This is
    /// present only for an explicit snapshot open and is shared by clones.
    snapshot_authority: Option<Arc<snapshot::Authority>>,

    /// Construction mode is kept as a type instead of being inferred from
    /// filesystem permissions. In particular, an explicit read-only snapshot
    /// remains distinct even when its installation root is writable.
    access: Access,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Discovery {
    System,
    FrozenCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Mutable,
    NaturallyReadOnly,
    ReadOnlySnapshot,
    FrozenCache,
}

impl Installation {
    /// Open a system root as an Installation type
    /// This will query the potential active state if found,
    /// and determine mutability from the trusted installation-root ownership
    /// and mode policy.
    pub fn open(root: impl Into<PathBuf>, cache_dir: Option<PathBuf>) -> Result<Self, Error> {
        Self::open_with_discovery(root.into(), cache_dir, Discovery::System)
    }

    /// Open an existing installation as a retained read-only snapshot.
    ///
    /// Unlike [`Self::open`], this mode never provisions, repairs, changes,
    /// syncs, or removes filesystem entries. The existing `.cast` directory
    /// and every applicable lockfile must already be safe. Shared locks and
    /// retained directory capabilities remain held by every clone.
    pub fn open_read_only(root: impl Into<PathBuf>, cache_dir: Option<PathBuf>) -> Result<Self, Error> {
        Self::open_read_only_with_lock_timeout(root.into(), cache_dir, snapshot::SHARED_LOCK_TIMEOUT)
    }

    fn open_read_only_with_lock_timeout(
        root: PathBuf,
        cache_dir: Option<PathBuf>,
        lock_timeout: std::time::Duration,
    ) -> Result<Self, Error> {
        if !root.exists() || !root.is_dir() {
            return Err(Error::RootInvalid);
        }

        let root_directory = open_installation_root_path(&root).map_err(|source| Error::ValidateRootDirectory {
            path: root.clone(),
            source,
        })?;
        let authority = snapshot::Authority::open(&root, &root_directory, cache_dir.as_deref(), lock_timeout)?;
        let active_state = read_state_id(&root_directory);

        if let Some(id) = &active_state {
            trace!("Active State ID: {id}");
        } else {
            warn!("Unable to discover Active State ID");
        }

        let installation = Self {
            root,
            mutability: Mutability::ReadOnly,
            active_state,
            cache_dir,
            system_model: None,
            root_directory: Arc::new(root_directory),
            mutable_namespace: None,
            _locks: vec![],
            snapshot_authority: Some(Arc::new(authority)),
            access: Access::ReadOnlySnapshot,
        };
        installation.revalidate_read_only_snapshot()?;
        Ok(installation)
    }

    /// Open only the persistent package cache used to materialize a frozen
    /// root.
    ///
    /// Unlike [`Self::open`], this never reads the installation's preliminary
    /// active-state witness. Neither opening mode evaluates authored
    /// `system.glu`; frozen callers must supply their complete repository and
    /// package intent explicitly.
    pub fn open_frozen(root: impl Into<PathBuf>, cache_dir: Option<PathBuf>) -> Result<Self, Error> {
        Self::open_with_discovery(root.into(), cache_dir, Discovery::FrozenCache)
    }

    fn open_with_discovery(root: PathBuf, cache_dir: Option<PathBuf>, discovery: Discovery) -> Result<Self, Error> {
        if !root.exists() || !root.is_dir() {
            return Err(Error::RootInvalid);
        }

        // Authenticate the installation root before provisioning any `.cast`
        // component below it. Every provisioning mutation is descriptor
        // relative and the root name is revalidated before and after the
        // remaining pathname-based open work. The readable root capability is
        // retained by Installation clones; migrating every activation path
        // beneath it remains tracked separately in PLAN.md.
        let root_directory = open_installation_root_path(&root).map_err(|source| Error::ValidateRootDirectory {
            path: root.clone(),
            source,
        })?;

        if let Some(dir) = &cache_dir
            && (!dir.exists() || !dir.is_dir())
        {
            return Err(Error::CacheInvalid);
        }

        let custom_cache_directory = cache_dir
            .as_deref()
            .map(|dir| {
                open_controlled_directory_path(dir)
                    .map(|file| ControlledDirectory {
                        file,
                        path: dir.to_owned(),
                    })
                    .map_err(|source| Error::ValidateCacheDirectory {
                        path: dir.to_owned(),
                        source,
                    })
            })
            .transpose()?;

        let root_metadata = root_directory
            .metadata()
            .map_err(|source| Error::ValidateRootDirectory {
                path: root.clone(),
                source,
            })?;
        let mutability = classify_installation_root_access(
            root_metadata.uid(),
            root_metadata.mode() & 0o7777,
            Uid::effective().as_raw(),
        );
        let provisioned_namespace = matches!(mutability, Mutability::ReadWrite)
            .then(|| ensure_dirs_exist(&root_directory, &root))
            .transpose()?;
        require_open_directories_still_named(
            &root,
            &root_directory,
            provisioned_namespace.as_ref().map(|directories| &directories.cast),
            custom_cache_directory.as_ref(),
        )?;

        trace!("Mutability: {mutability}");
        trace!("Root dir: {root:?}");

        // Get exclusive access to work within these directories
        let _locks = match &provisioned_namespace {
            Some(directories) => acquire_controlled_locks(&directories.cast, custom_cache_directory.as_ref())?,
            None => vec![],
        };

        let active_state = match discovery {
            Discovery::System => {
                let active_state = read_state_id(&root_directory);

                if let Some(id) = &active_state {
                    trace!("Active State ID: {id}");
                } else {
                    warn!("Unable to discover Active State ID");
                }

                active_state
            }
            Discovery::FrozenCache => None,
        };

        require_open_directories_still_named(
            &root,
            &root_directory,
            provisioned_namespace.as_ref().map(|directories| &directories.cast),
            custom_cache_directory.as_ref(),
        )?;

        let mutable_namespace = provisioned_namespace.map(|directories| {
            let global_lock = _locks
                .first()
                .expect("a provisioned Cast namespace has one global lock")
                .clone();
            Arc::new(mutable_namespace::Authority::new(directories, global_lock))
        });
        if let Some(authority) = &mutable_namespace {
            authority.revalidate(&root, &root_directory)?;
        }

        Ok(Self {
            root,
            mutability,
            active_state,
            cache_dir,
            system_model: None,
            root_directory: Arc::new(root_directory),
            mutable_namespace,
            _locks,
            snapshot_authority: None,
            access: match (discovery, mutability) {
                (Discovery::System, Mutability::ReadWrite) => Access::Mutable,
                (Discovery::System, Mutability::ReadOnly) => Access::NaturallyReadOnly,
                (Discovery::FrozenCache, _) => Access::FrozenCache,
            },
        })
    }

    pub(crate) fn is_frozen_cache(&self) -> bool {
        matches!(self.access, Access::FrozenCache)
    }

    /// Return whether this installation owns explicit shared snapshot
    /// authority rather than mutable or frozen-cache authority.
    pub(crate) fn is_read_only_snapshot(&self) -> bool {
        matches!(self.access, Access::ReadOnlySnapshot)
    }

    /// Return whether this installation was opened as a writable system,
    /// rather than inferring mutation authority from a path or boolean.
    pub(crate) fn is_mutable_system(&self) -> bool {
        matches!(self.access, Access::Mutable)
    }

    /// Borrow the exact installation-root inode authenticated before locks,
    /// database discovery, and client construction.
    pub(crate) fn root_directory(&self) -> &std::fs::File {
        &self.root_directory
    }

    /// Prove that the public installation-root name still denotes the retained
    /// capability. Call this before and after any descriptor-relative namespace
    /// mutation whose result is intended to be reachable through `root`.
    pub(crate) fn revalidate_root_directory(&self) -> Result<(), Error> {
        require_named_installation_root(&self.root, &self.root_directory).map_err(|source| {
            Error::ValidateRootDirectory {
                path: self.root.clone(),
                source,
            }
        })
    }

    /// Deadline-aware root-name revalidation for finite pre-effect capture.
    ///
    /// The retained root policy and public-name identity checks are identical
    /// to [`Self::revalidate_root_directory`]. Only the filesystem substrate
    /// differs: every open and ACL probe observes `deadline` and inherits the
    /// shared finite interrupted-syscall retry ceiling.
    pub(crate) fn revalidate_root_directory_until(&self, deadline: Instant) -> Result<(), Error> {
        require_named_installation_root_until(&self.root, &self.root_directory, deadline).map_err(|source| {
            Error::ValidateRootDirectory {
                path: self.root.clone(),
                source,
            }
        })
    }

    /// Revalidate the exact mutable startup namespace retained by every clone.
    pub(crate) fn revalidate_mutable_namespace(&self) -> Result<(), Error> {
        if !self.is_mutable_system() {
            return Err(Error::MutableSystemNamespaceRequired);
        }
        self.mutable_namespace
            .as_deref()
            .ok_or(Error::MutableSystemNamespaceRequired)?
            .revalidate(&self.root, &self.root_directory)
    }

    /// Produce an anchored SQLite location below the retained database
    /// directory. Callers must keep the returned anchor alive in the database
    /// object and revalidate the namespace after SQLite has opened the URL.
    pub(crate) fn mutable_database_location(&self, kind: DatabaseKind) -> Result<MutableDatabaseLocation, Error> {
        self.revalidate_mutable_namespace()?;
        let location = self
            .mutable_namespace
            .as_deref()
            .ok_or(Error::MutableSystemNamespaceRequired)?
            .database_location(kind);
        self.revalidate_mutable_namespace()?;
        location
    }

    /// Borrow the retained `.cast` descriptor used by startup journal work.
    /// The public name is authenticated before the borrow and again by the
    /// surrounding startup stage after the descriptor-relative work.
    pub(crate) fn retained_mutable_cast_directory(&self) -> Result<&std::fs::File, Error> {
        self.revalidate_mutable_namespace()?;
        Ok(self
            .mutable_namespace
            .as_deref()
            .ok_or(Error::MutableSystemNamespaceRequired)?
            .cast_directory())
    }

    /// Revalidate every retained component that authorizes an explicit
    /// read-only snapshot. Mutable and frozen-cache installations cannot be
    /// accidentally treated as snapshot authority.
    pub(crate) fn revalidate_read_only_snapshot(&self) -> Result<(), Error> {
        if !self.is_read_only_snapshot() {
            return Err(Error::ReadOnlySnapshotAuthorityRequired);
        }
        let authority = self
            .snapshot_authority
            .as_deref()
            .ok_or(Error::ReadOnlySnapshotAuthorityRequired)?;
        authority.revalidate(&self.root, &self.root_directory)
    }

    #[allow(dead_code)] // consumed by the next read-only-client slice
    pub(crate) fn open_read_only_database(&self, kind: DatabaseKind) -> Result<ReadOnlyDatabaseFile, Error> {
        self.revalidate_read_only_snapshot()?;
        let database = self
            .snapshot_authority
            .as_deref()
            .ok_or(Error::ReadOnlySnapshotAuthorityRequired)?
            .open_database(kind)?;
        self.revalidate_read_only_snapshot()?;
        Ok(database)
    }

    #[allow(dead_code)] // consumed by the next read-only-client slice
    pub(crate) fn revalidate_read_only_database(&self, database: &ReadOnlyDatabaseFile) -> Result<(), Error> {
        self.revalidate_read_only_snapshot()?;
        self.snapshot_authority
            .as_deref()
            .ok_or(Error::ReadOnlySnapshotAuthorityRequired)?
            .revalidate_database(database)?;
        self.revalidate_read_only_snapshot()
    }

    #[allow(dead_code)] // consumed by the next read-only-client slice
    pub(crate) fn read_read_only_database_image(
        &self,
        database: &ReadOnlyDatabaseFile,
        max_bytes: usize,
    ) -> Result<Box<[u8]>, Error> {
        self.revalidate_read_only_snapshot()?;
        let image = self
            .snapshot_authority
            .as_deref()
            .ok_or(Error::ReadOnlySnapshotAuthorityRequired)?
            .read_database_image(database, max_bytes)?;
        self.revalidate_read_only_snapshot()?;
        Ok(image)
    }

    /// Return true if we lack write access
    pub fn read_only(&self) -> bool {
        matches!(self.mutability, Mutability::ReadOnly)
    }

    // Helper to form paths
    fn cast_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.root.join(".cast").join(path)
    }

    /// Build a database path relative to the Cast root
    pub fn db_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.cast_path("db").join(path)
    }

    /// Build a cache path relative to the Cast root, or
    /// from the custom cache dir, if provided
    pub fn cache_path(&self, path: impl AsRef<Path>) -> PathBuf {
        if let Some(dir) = &self.cache_dir {
            dir.join(path)
        } else {
            self.cast_path("cache").join(path)
        }
    }

    /// Build an asset path relative to the Cast root
    pub fn assets_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.cast_path("assets").join(path)
    }

    /// Build a repo path relative to the root
    pub fn repo_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.cast_path("repo").join(path)
    }

    /// Build a path relative to the Cast system roots tree
    pub fn root_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.cast_path("root").join(path)
    }

    /// Return the private directory for failed trees that must not be exposed
    /// as bootable or prunable system roots.
    pub(crate) fn state_quarantine_dir(&self) -> PathBuf {
        self.cast_path("quarantine")
    }

    /// Build a staging path for in-progress system root transactions
    pub fn staging_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.root_path("staging").join(path)
    }

    /// Return the staging directory itself
    pub fn staging_dir(&self) -> PathBuf {
        self.root_path("staging")
    }

    /// Return the container dir itself
    pub fn isolation_dir(&self) -> PathBuf {
        self.root_path("isolation")
    }

    /// Build a container path for isolated triggers
    pub fn isolation_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.root_path("isolation").join(path)
    }

    /// Path to the user-authored desired system intent.
    pub fn system_intent_path(&self) -> PathBuf {
        system_model::intent_path(&self.root)
    }
}

include!("installation/locking.rs");
include!("installation/state_discovery.rs");
include!("installation/directory_control.rs");
include!("installation/cache_tag.rs");
/// Errors specific to a target installation filesystem
#[derive(Debug, Error)]
pub enum Error {
    #[error("Root is invalid")]
    RootInvalid,
    #[error("Cache dir is invalid")]
    CacheInvalid,
    #[error("validate installation root directory `{}`", path.display())]
    ValidateRootDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("prepare Cast directory `{}`", path.display())]
    PrepareDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("validate custom cache directory `{}`", path.display())]
    ValidateCacheDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("prepare cache-directory tag `{}`", path.display())]
    PrepareCachedirTag {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("prepare installation lockfile `{}`", path.display())]
    PrepareLockfile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("acquiring lockfile")]
    Lockfile(#[from] lockfile::Error),
    #[error("open existing read-only snapshot directory `{}`", path.display())]
    OpenReadOnlySnapshotDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open existing read-only snapshot lockfile `{}`", path.display())]
    OpenReadOnlySnapshotLockfile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("timed out after {timeout:?} acquiring shared read-only snapshot lock `{}`", path.display())]
    ReadOnlySnapshotLockTimeout {
        path: PathBuf,
        timeout: std::time::Duration,
    },
    #[error("open existing read-only database `{}`", path.display())]
    OpenReadOnlyDatabase {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read-only database identity changed: {}", path.display())]
    ReadOnlyDatabaseChanged { path: PathBuf },
    #[error("read-only database has unsupported SQLite sidecar evidence: {}", path.display())]
    ReadOnlyDatabaseSidecar { path: PathBuf },
    #[error("read-only database exceeds the {limit}-byte image bound: {} ({size} bytes)", path.display())]
    ReadOnlyDatabaseTooLarge { path: PathBuf, size: u64, limit: usize },
    #[error("read stable read-only database image `{}`", path.display())]
    ReadOnlyDatabaseImage {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read-only database metadata or length changed while imaging: {}", path.display())]
    ReadOnlyDatabaseImageChanged { path: PathBuf },
    #[error("explicit read-only snapshot authority is required")]
    ReadOnlySnapshotAuthorityRequired,
    #[error("retained mutable system namespace authority is required")]
    MutableSystemNamespaceRequired,
}

#[cfg(test)]
mod tests;
