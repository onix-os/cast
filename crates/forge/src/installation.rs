// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Encapsulation of a target installation filesystem

use std::{
    ffi::{CStr, CString, OsStr},
    fmt,
    io::{self, Read as _, Seek as _, SeekFrom, Write as _},
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
};

use fs_err as fs;
use log::{trace, warn};
use nix::unistd::Uid;
use thiserror::Error;
use tui::Styled;

use crate::{
    linux_fs::chmod_path_descriptor,
    state,
    system_model::{self, LoadedSystemModel},
};

mod lockfile;

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

    /// If present, the currently active system state Id
    pub active_state: Option<state::Id>,

    /// Custom cache directory location,
    /// otherwise derived from root
    pub cache_dir: Option<PathBuf>,

    /// If defined, the user-authored desired system intent of the installation.
    pub system_model: Option<LoadedSystemModel>,

    /// Acquired locks that guarantee exclusive access
    /// to the installation for mutable operations
    _locks: Vec<lockfile::Lock>,

    /// Whether host system intent and active state discovery was permitted
    /// while opening this installation.
    discovery: Discovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Discovery {
    System,
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

    /// Open only the persistent package cache used to materialize a frozen
    /// root.
    ///
    /// Unlike [`Self::open`], this never reads the installation's active state
    /// or authored `system.glu`. Frozen callers must supply their complete
    /// repository and package intent explicitly.
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
        // remaining pathname-based open work. Retaining root authority across
        // later transactions belongs to the descriptor-rooted activation work
        // tracked separately in PLAN.md.
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
        let cast_directory = matches!(mutability, Mutability::ReadWrite)
            .then(|| ensure_dirs_exist(&root_directory, &root))
            .transpose()?;
        require_open_directories_still_named(
            &root,
            &root_directory,
            cast_directory.as_ref(),
            custom_cache_directory.as_ref(),
        )?;

        trace!("Mutability: {mutability}");
        trace!("Root dir: {root:?}");

        // Get exclusive access to work within these directories
        let _locks = match &cast_directory {
            Some(cast_directory) => acquire_controlled_locks(cast_directory, custom_cache_directory.as_ref())?,
            None => vec![],
        };

        let (active_state, system_model) = match discovery {
            Discovery::System => {
                let active_state = read_state_id(&root);

                if let Some(id) = &active_state {
                    trace!("Active State ID: {id}");
                } else {
                    warn!("Unable to discover Active State ID");
                }

                let system_model =
                    system_model::load(&system_model::intent_path(&root)).map_err(Error::LoadSystemModel)?;
                (active_state, system_model)
            }
            Discovery::FrozenCache => (None, None),
        };

        require_open_directories_still_named(
            &root,
            &root_directory,
            cast_directory.as_ref(),
            custom_cache_directory.as_ref(),
        )?;

        Ok(Self {
            root,
            mutability,
            active_state,
            cache_dir,
            system_model,
            _locks,
            discovery,
        })
    }

    pub(crate) fn is_frozen_cache(&self) -> bool {
        matches!(self.discovery, Discovery::FrozenCache)
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

fn require_open_directories_still_named(
    root_path: &Path,
    root: &std::fs::File,
    cast: Option<&ControlledDirectory>,
    custom_cache: Option<&ControlledDirectory>,
) -> Result<(), Error> {
    require_named_installation_root(root_path, root).map_err(|source| Error::ValidateRootDirectory {
        path: root_path.to_owned(),
        source,
    })?;
    if let Some(cast) = cast {
        require_named_controlled_child(root, CAST_DIRECTORY_NAME, cast).map_err(|source| Error::PrepareDirectory {
            path: cast.path.clone(),
            source,
        })?;
    }
    if let Some(cache) = custom_cache {
        require_named_controlled_directory_path(&cache.path, &cache.file).map_err(|source| {
            Error::ValidateCacheDirectory {
                path: cache.path.clone(),
                source,
            }
        })?;
    }
    Ok(())
}

fn acquire_controlled_locks(
    cast: &ControlledDirectory,
    custom_cache: Option<&ControlledDirectory>,
) -> Result<Vec<lockfile::Lock>, Error> {
    let mut locks = Vec::with_capacity(1 + usize::from(custom_cache.is_some()));
    locks.push(acquire_controlled_lock(
        &cast.file,
        &cast.path.join(LOCKFILE_NAME.to_string_lossy().as_ref()),
        format!("{} another process is using the Cast root", "Blocking".yellow().bold()),
    )?);

    if let Some(cache) = custom_cache {
        locks.push(acquire_controlled_lock(
            &cache.file,
            &cache.path.join(LOCKFILE_NAME.to_string_lossy().as_ref()),
            format!("{} another process is using the cache dir", "Blocking".yellow().bold()),
        )?);
    }
    Ok(locks)
}

fn acquire_controlled_lock(
    directory: &std::fs::File,
    path: &Path,
    block_message: impl fmt::Display,
) -> Result<lockfile::Lock, Error> {
    let (file, expected_identity) =
        open_controlled_lockfile(directory, path).map_err(|source| Error::PrepareLockfile {
            path: path.to_owned(),
            source,
        })?;
    let lock = lockfile::acquire_file(file, block_message)?;
    openat2_file(
        directory.as_raw_fd(),
        LOCKFILE_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .and_then(|file| {
        require_controlled_lockfile(&file, path)?;
        require_lockfile_identity(expected_identity, lockfile_identity(&file)?)
    })
    .map_err(|source| Error::PrepareLockfile {
        path: path.to_owned(),
        source,
    })?;
    Ok(lock)
}

fn open_controlled_lockfile(directory: &std::fs::File, path: &Path) -> io::Result<(std::fs::File, (u64, u64))> {
    loop {
        match openat2_file(
            directory.as_raw_fd(),
            LOCKFILE_NAME,
            nix::libc::O_RDWR
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK
                | nix::libc::O_CREAT
                | nix::libc::O_EXCL,
            LOCKFILE_MODE,
            controlled_resolution(),
        ) {
            Ok(file) => {
                require_fresh_lockfile(&file, path)?;
                file.set_permissions(std::fs::Permissions::from_mode(LOCKFILE_MODE))?;
                require_controlled_lockfile(&file, path)?;
                file.sync_all()?;
                directory.sync_all()?;
                let identity = lockfile_identity(&file)?;
                let named = openat2_file(
                    directory.as_raw_fd(),
                    LOCKFILE_NAME,
                    nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                    0,
                    controlled_resolution(),
                )?;
                require_controlled_lockfile(&named, path)?;
                require_lockfile_identity(identity, lockfile_identity(&named)?)?;
                return Ok((file, identity));
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                let probe = match openat2_file(
                    directory.as_raw_fd(),
                    LOCKFILE_NAME,
                    nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                    0,
                    controlled_resolution(),
                ) {
                    Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                    result => result?,
                };
                if require_controlled_lockfile(&probe, path).is_err() {
                    require_fresh_lockfile(&probe, path)?;
                    chmod_path_descriptor(&probe, LOCKFILE_MODE)?;
                    require_controlled_lockfile(&probe, path)?;
                }
                let identity = lockfile_identity(&probe)?;
                let file = match openat2_file(
                    directory.as_raw_fd(),
                    LOCKFILE_NAME,
                    nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
                    0,
                    controlled_resolution(),
                ) {
                    Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                    result => result?,
                };
                require_controlled_lockfile(&file, path)?;
                require_lockfile_identity(identity, lockfile_identity(&file)?)?;
                file.sync_all()?;
                directory.sync_all()?;
                return Ok((file, identity));
            }
            Err(source) => return Err(source),
        }
    }
}

fn require_fresh_lockfile(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if metadata.file_type().is_file()
        && metadata.uid() == Uid::effective().as_raw()
        && metadata.nlink() == 1
        && mode & !LOCKFILE_MODE == 0
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "fresh lockfile is not recoverable owner-only creation residue: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ))
    }
}

fn require_controlled_lockfile(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if metadata.file_type().is_file()
        && metadata.uid() == Uid::effective().as_raw()
        && metadata.nlink() == 1
        && mode & 0o7000 == 0
        && mode & 0o022 == 0
        && mode & 0o600 == 0o600
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "lockfile is not one safe owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ))
    }
}

fn lockfile_identity(file: &std::fs::File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

fn require_lockfile_identity(expected: (u64, u64), actual: (u64, u64)) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::other("lockfile inode changed during acquisition"))
    }
}

/// In older versions of Cast, the `/usr` entry was a symlink
/// to an active state. In newer versions, the state is recorded
/// within the installation tree. (`/usr/.stateID`)
fn read_state_id(root: &Path) -> Option<state::Id> {
    let usr_path = root.join("usr");
    let state_path = root.join("usr").join(".stateID");

    if let Some(id) = fs::read_to_string(state_path).ok().and_then(|s| s.parse::<i32>().ok()) {
        return Some(state::Id::from(id));
    } else if let Ok(usr_target) = usr_path.read_link() {
        return read_legacy_state_id(&usr_target);
    }

    None
}

// Legacy `/usr` link support
fn read_legacy_state_id(usr_target: &Path) -> Option<state::Id> {
    if usr_target.ends_with("usr") {
        let parent = usr_target.parent()?;
        let base = parent.file_name()?;
        let id = base.to_str()?.parse::<i32>().ok()?;

        return Some(state::Id::from(id));
    }

    None
}

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const LOCKFILE_MODE: u32 = 0o600;
const CAST_DIRECTORY_NAME: &CStr = c".cast";
const LOCKFILE_NAME: &CStr = c".cast-lockfile";
const CACHEDIR_TAG_MODE: u32 = 0o644;
const CACHEDIR_TAG_TEMPORARY_MODE: u32 = 0o600;
const POSIX_DEFAULT_ACL_XATTR: &CStr = c"system.posix_acl_default";
const CACHEDIR_TAG_NAME: &CStr = c"CACHEDIR.TAG";
const CACHEDIR_TAG_TEMPORARY_NAME: &CStr = c".CACHEDIR.TAG.cast-tmp";
const CACHEDIR_TAG_CONTENTS: &[u8] = br#"Signature: 8a477f597d28d172789f06886806bc55
# This file is a cache directory tag created by Cast.
# For information about cache directory tags see https://bford.info/cachedir/"#;

#[derive(Debug)]
struct ControlledDirectory {
    file: std::fs::File,
    path: PathBuf,
}

/// Ensures Cast directories are created without allowing the cache and asset
/// capability roots to inherit a permissive process-global umask.
fn ensure_dirs_exist(root_directory: &std::fs::File, root: &Path) -> Result<ControlledDirectory, Error> {
    let cast_path = root.join(".cast");
    let cast = ensure_controlled_child(root_directory, OsStr::new(".cast"), &cast_path).map_err(|source| {
        Error::PrepareDirectory {
            path: cast_path.clone(),
            source,
        }
    })?;

    let cache_path = cast_path.join("cache");
    let cache = ensure_controlled_child(&cast.file, OsStr::new("cache"), &cache_path).map_err(|source| {
        Error::PrepareDirectory {
            path: cache_path.clone(),
            source,
        }
    })?;
    let assets_path = cast_path.join("assets");
    ensure_controlled_child(&cast.file, OsStr::new("assets"), &assets_path).map_err(|source| {
        Error::PrepareDirectory {
            path: assets_path,
            source,
        }
    })?;
    let quarantine_path = cast_path.join("quarantine");
    ensure_controlled_child(&cast.file, OsStr::new("quarantine"), &quarantine_path).map_err(|source| {
        Error::PrepareDirectory {
            path: quarantine_path,
            source,
        }
    })?;

    // Build the remaining fixed directory topology through the same pinned,
    // durable creation boundary. Existing safe shared-readable modes are
    // preserved, while new entries and restrictive-umask residue become 0700.
    for name in ["db", "repo"] {
        let path = cast_path.join(name);
        ensure_controlled_child(&cast.file, OsStr::new(name), &path)
            .map_err(|source| Error::PrepareDirectory { path, source })?;
    }
    let roots_path = cast_path.join("root");
    let roots = ensure_controlled_child(&cast.file, OsStr::new("root"), &roots_path).map_err(|source| {
        Error::PrepareDirectory {
            path: roots_path.clone(),
            source,
        }
    })?;
    for name in ["staging", "isolation"] {
        let path = roots_path.join(name);
        ensure_controlled_child(&roots.file, OsStr::new(name), &path)
            .map_err(|source| Error::PrepareDirectory { path, source })?;
    }

    ensure_cachedir_tag(&cache).map_err(|source| Error::PrepareCachedirTag {
        path: cache_path.join("CACHEDIR.TAG"),
        source,
    })?;
    Ok(cast)
}

fn ensure_controlled_child(parent: &std::fs::File, name: &OsStr, path: &Path) -> io::Result<ControlledDirectory> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory name contains NUL"))?;
    mkdirat_if_absent(parent.as_raw_fd(), name.as_c_str(), PRIVATE_DIRECTORY_MODE)?;

    let pinned = openat2_file(
        parent.as_raw_fd(),
        name.as_c_str(),
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    if requires_private_mode_recovery(&pinned, path)? {
        // Normalize only this retained inode. This also completes a prior
        // mkdir/open crash whose restrictive umask exposed an owner-only
        // mode below 0700. Unsafe pre-existing evidence is rejected above
        // and is never chmod-laundered.
        chmod_path_descriptor(&pinned, PRIVATE_DIRECTORY_MODE)?;
    }
    require_controlled_directory(&pinned, path)?;

    let directory = openat2_file(
        parent.as_raw_fd(),
        name.as_c_str(),
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
    // A readable descriptor is required for directory fsync on Linux. Always
    // sync both descriptors so retrying after a crash also completes an entry
    // whose earlier parent-directory sync may not have reached stable storage.
    directory.sync_all()?;
    parent.sync_all()?;

    let named = openat2_file(
        parent.as_raw_fd(),
        name.as_c_str(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_controlled_directory(&named, path)?;
    require_no_default_acl(&directory, path)?;
    require_no_default_acl(&named, path)?;
    require_same_directory(&directory, &named, path)?;
    Ok(ControlledDirectory {
        file: directory,
        path: path.to_owned(),
    })
}

fn mkdirat_if_absent(parent: RawFd, name: &CStr, mode: u32) -> io::Result<()> {
    loop {
        // SAFETY: the parent descriptor and single NUL-terminated component
        // remain live. mkdirat never follows the final component.
        if unsafe { nix::libc::mkdirat(parent, name.as_ptr(), mode) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::AlreadyExists => return Ok(()),
            _ => return Err(source),
        }
    }
}

fn requires_private_mode_recovery(file: &std::fs::File, path: &Path) -> io::Result<bool> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;

    // Existing safe 0750/0755-style capability roots remain untouched.
    if require_controlled_directory_metadata(&metadata, path, Uid::effective().as_raw()).is_ok() {
        return Ok(false);
    }

    // A mkdir requested 0700. Under a restrictive umask, a crash can expose
    // only a same-owner directory whose mode is a strict subset of 0700.
    if metadata.file_type().is_dir()
        && metadata.uid() == Uid::effective().as_raw()
        && mode & !PRIVATE_DIRECTORY_MODE == 0
    {
        return Ok(true);
    }

    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "capability root is neither safe nor a recoverable owner-only mkdir residue: {} (uid={}, mode={mode:04o})",
            path.display(),
            metadata.uid()
        ),
    ))
}

fn require_controlled_directory(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    require_controlled_directory_metadata(&metadata, path, Uid::effective().as_raw())
}

fn require_controlled_directory_metadata(
    metadata: &std::fs::Metadata,
    path: &Path,
    expected_owner: u32,
) -> io::Result<()> {
    // POSIX access-ACL effective permissions are reflected through the owning
    // group class mask in st_mode, so shared write access is rejected here as
    // 0020. Default ACLs are a separate inherited authority and are rejected
    // on the retained readable descriptor below.
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != expected_owner
        || mode & 0o7000 != 0
        || mode & 0o700 != 0o700
        || mode & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "capability root is not one safe owner-controlled directory: {} (uid={}, mode={mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ));
    }
    Ok(())
}

fn require_no_default_acl(file: &std::fs::File, path: &Path) -> io::Result<()> {
    loop {
        // SAFETY: `file` and the static xattr name remain live. A null value
        // with size zero is the documented existence/size query and does not
        // copy attribute bytes into userspace.
        let result = unsafe {
            nix::libc::fgetxattr(
                file.as_raw_fd(),
                POSIX_DEFAULT_ACL_XATTR.as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        };
        if result >= 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "capability directory carries an inheritable POSIX default ACL: {}",
                    path.display()
                ),
            ));
        }

        let source = io::Error::last_os_error();
        match source.raw_os_error() {
            Some(nix::libc::EINTR) => {}
            Some(nix::libc::ENODATA) | Some(nix::libc::EOPNOTSUPP) => return Ok(()),
            _ => return Err(source),
        }
    }
}

fn require_same_directory(first: &std::fs::File, second: &std::fs::File, path: &Path) -> io::Result<()> {
    let first = first.metadata()?;
    let second = second.metadata()?;
    if (first.dev(), first.ino()) != (second.dev(), second.ino()) {
        return Err(io::Error::other(format!(
            "capability root changed while opening: {}",
            path.display()
        )));
    }
    Ok(())
}

fn open_directory_path(path: &Path) -> io::Result<std::fs::File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory path contains NUL"))?;
    openat2_file(
        nix::libc::AT_FDCWD,
        &path,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )
}

fn open_controlled_directory_path(path: &Path) -> io::Result<std::fs::File> {
    let pinned = open_directory_path(path)?;
    require_controlled_directory(&pinned, path)?;

    let encoded = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory path contains NUL"))?;
    let directory = openat2_file(
        nix::libc::AT_FDCWD,
        &encoded,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )?;
    require_controlled_directory(&directory, path)?;
    require_no_default_acl(&directory, path)?;
    require_same_directory(&pinned, &directory, path)?;
    Ok(directory)
}

fn require_named_controlled_directory_path(path: &Path, retained: &std::fs::File) -> io::Result<()> {
    require_controlled_directory(retained, path)?;
    require_no_default_acl(retained, path)?;
    let named = open_controlled_directory_path(path)?;
    require_same_directory(retained, &named, path)
}

fn require_named_controlled_child(
    parent: &std::fs::File,
    name: &CStr,
    retained: &ControlledDirectory,
) -> io::Result<()> {
    require_controlled_directory(&retained.file, &retained.path)?;
    require_no_default_acl(&retained.file, &retained.path)?;
    let named = openat2_file(
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
    require_controlled_directory(&named, &retained.path)?;
    require_no_default_acl(&named, &retained.path)?;
    require_same_directory(&retained.file, &named, &retained.path)
}

fn open_installation_root_path(path: &Path) -> io::Result<std::fs::File> {
    let pinned = open_directory_path(path)?;
    require_installation_root(&pinned, path)?;

    let encoded = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory path contains NUL"))?;
    let directory = openat2_file(
        nix::libc::AT_FDCWD,
        &encoded,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )?;
    require_installation_root(&directory, path)?;
    require_no_default_acl(&directory, path)?;
    require_same_directory(&pinned, &directory, path)?;
    Ok(directory)
}

fn require_named_installation_root(path: &Path, retained: &std::fs::File) -> io::Result<()> {
    require_installation_root(retained, path)?;
    require_no_default_acl(retained, path)?;
    let named = open_installation_root_path(path)?;
    require_same_directory(retained, &named, path)
}

fn require_installation_root(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    require_installation_root_policy(
        metadata.file_type().is_dir(),
        metadata.uid(),
        metadata.mode() & 0o7777,
        Uid::effective().as_raw(),
        path,
    )
}

fn require_installation_root_policy(
    is_directory: bool,
    owner: u32,
    mode: u32,
    effective_owner: u32,
    path: &Path,
) -> io::Result<()> {
    if !is_directory
        || (owner != effective_owner && owner != 0)
        || mode & 0o7000 != 0
        || mode & 0o022 != 0
        || mode & 0o500 != 0o500
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "installation root is not a safe effective-user- or root-owned readable directory: {} (uid={owner}, mode={mode:04o})",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn classify_installation_root_access(owner: u32, mode: u32, effective_owner: u32) -> Mutability {
    if owner == effective_owner && mode & 0o200 != 0 {
        Mutability::ReadWrite
    } else {
        // A root-owned installation opened by an unprivileged caller is
        // intentionally read-only even if some ambient credential mechanism
        // would make a pathname access probe succeed. Provisioning authority
        // is derived only from the authenticated owner and owner-write bit.
        Mutability::ReadOnly
    }
}

fn openat2_file(dirfd: RawFd, path: &CStr, flags: i32, mode: u32, resolve: u64) -> io::Result<std::fs::File> {
    // SAFETY: zero is valid for every public `open_how` field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: the descriptor, C string, and open_how remain live. Success
    // returns one fresh descriptor owned below.
    let descriptor = loop {
        let descriptor = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_openat2,
                dirfd,
                path.as_ptr(),
                &how,
                size_of::<nix::libc::open_how>(),
            )
        };
        if descriptor != -1 {
            break descriptor;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    };
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(std::fs::File::from(descriptor))
}

fn controlled_resolution() -> u64 {
    (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CachedirTagIdentity {
    device: u64,
    inode: u64,
}

/// Publish a complete cache tag atomically. The canonical name is never an
/// incomplete file: bytes and mode are finalized and fsynced on a private
/// temporary inode before `RENAME_NOREPLACE` exposes it.
fn ensure_cachedir_tag(cache: &ControlledDirectory) -> io::Result<()> {
    let canonical_path = cache.path.join("CACHEDIR.TAG");
    if let Some(mut canonical) = open_cachedir_tag(cache, CACHEDIR_TAG_NAME)? {
        sync_exact_cachedir_tag(&mut canonical, &canonical_path)?;
        cleanup_cachedir_tag_residue(cache)?;
        return cache.file.sync_all();
    }

    let Some(mut temporary) = prepare_cachedir_tag_temporary(cache)? else {
        let mut canonical = open_cachedir_tag(cache, CACHEDIR_TAG_NAME)?
            .ok_or_else(|| io::Error::other("cache tag appeared and then disappeared during preparation"))?;
        sync_exact_cachedir_tag(&mut canonical, &canonical_path)?;
        return cache.file.sync_all();
    };
    let identity = cachedir_tag_identity(&temporary)?;
    let temporary_path = cache.path.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());

    if require_exact_cachedir_tag(&mut temporary, &temporary_path, CACHEDIR_TAG_MODE).is_err() {
        let build = (|| {
            temporary.set_len(0)?;
            temporary.seek(SeekFrom::Start(0))?;
            temporary.set_permissions(std::fs::Permissions::from_mode(CACHEDIR_TAG_TEMPORARY_MODE))?;
            temporary.write_all(CACHEDIR_TAG_CONTENTS)?;
            temporary.sync_all()?;
            temporary.set_permissions(std::fs::Permissions::from_mode(CACHEDIR_TAG_MODE))?;
            temporary.sync_all()?;
            require_exact_cachedir_tag(&mut temporary, &temporary_path, CACHEDIR_TAG_MODE)
        })();
        if let Err(source) = build {
            return Err(cleanup_cachedir_tag_after_failure(cache, identity, source));
        }
    }
    // A complete residue may have been left before its creator reached
    // fsync. Complete the inode durability step on every retry before the
    // atomic namespace publication.
    if let Err(source) = temporary.sync_all() {
        return Err(cleanup_cachedir_tag_after_failure(cache, identity, source));
    }

    match renameat2_noreplace(cache.file.as_raw_fd(), CACHEDIR_TAG_TEMPORARY_NAME, CACHEDIR_TAG_NAME) {
        Ok(()) => {
            let mut canonical = open_cachedir_tag(cache, CACHEDIR_TAG_NAME)?
                .ok_or_else(|| io::Error::other("published cache tag disappeared"))?;
            require_same_cachedir_tag(identity, cachedir_tag_identity(&canonical)?)?;
            sync_exact_cachedir_tag(&mut canonical, &canonical_path)?;
            cache.file.sync_all()
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            cleanup_cachedir_tag_identity(cache, identity)?;
            let mut canonical = open_cachedir_tag(cache, CACHEDIR_TAG_NAME)?
                .ok_or_else(|| io::Error::other("competing cache tag disappeared"))?;
            sync_exact_cachedir_tag(&mut canonical, &canonical_path)?;
            cache.file.sync_all()
        }
        Err(source) => Err(cleanup_cachedir_tag_after_failure(cache, identity, source)),
    }
}

fn prepare_cachedir_tag_temporary(cache: &ControlledDirectory) -> io::Result<Option<std::fs::File>> {
    loop {
        if open_cachedir_tag(cache, CACHEDIR_TAG_NAME)?.is_some() {
            return Ok(None);
        }

        let flags = nix::libc::O_RDWR
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_CREAT
            | nix::libc::O_EXCL;
        match openat2_file(
            cache.file.as_raw_fd(),
            CACHEDIR_TAG_TEMPORARY_NAME,
            flags,
            CACHEDIR_TAG_TEMPORARY_MODE,
            controlled_resolution(),
        ) {
            Ok(file) => {
                let identity = cachedir_tag_identity(&file)?;
                if let Err(source) = flock_exclusive(&file)
                    .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(CACHEDIR_TAG_TEMPORARY_MODE)))
                {
                    return Err(cleanup_cachedir_tag_after_failure(cache, identity, source));
                }
                return Ok(Some(file));
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                let Some((mut file, identity)) = lock_existing_cachedir_tag_temporary(cache)? else {
                    continue;
                };
                let path = cache.path.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
                if require_exact_cachedir_tag(&mut file, &path, CACHEDIR_TAG_MODE).is_ok() {
                    return Ok(Some(file));
                }
                cleanup_cachedir_tag_identity(cache, identity)?;
            }
            Err(source) => return Err(source),
        }
    }
}

fn cleanup_cachedir_tag_residue(cache: &ControlledDirectory) -> io::Result<()> {
    let Some((_file, identity)) = lock_existing_cachedir_tag_temporary(cache)? else {
        return Ok(());
    };
    cleanup_cachedir_tag_identity(cache, identity)
}

fn lock_existing_cachedir_tag_temporary(
    cache: &ControlledDirectory,
) -> io::Result<Option<(std::fs::File, CachedirTagIdentity)>> {
    let pinned = match openat2_file(
        cache.file.as_raw_fd(),
        CACHEDIR_TAG_TEMPORARY_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    let mode = require_safe_cachedir_tag_temporary(&pinned, &cache.path.join(".CACHEDIR.TAG.cast-tmp"))?;
    if mode != CACHEDIR_TAG_TEMPORARY_MODE && mode != CACHEDIR_TAG_MODE {
        chmod_path_descriptor(&pinned, CACHEDIR_TAG_TEMPORARY_MODE)?;
    }
    let identity = cachedir_tag_identity(&pinned)?;
    let file = openat2_file(
        cache.file.as_raw_fd(),
        CACHEDIR_TAG_TEMPORARY_NAME,
        nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_same_cachedir_tag(identity, cachedir_tag_identity(&file)?)?;
    flock_exclusive(&file)?;

    let named = match openat2_file(
        cache.file.as_raw_fd(),
        CACHEDIR_TAG_TEMPORARY_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    require_same_cachedir_tag(identity, cachedir_tag_identity(&named)?)?;
    Ok(Some((file, identity)))
}

fn open_cachedir_tag(cache: &ControlledDirectory, name: &CStr) -> io::Result<Option<std::fs::File>> {
    // Pin and validate the final inode without opening it for data access.
    // O_PATH is side-effect-free for devices, FIFOs, sockets, directories,
    // and symlinks, all of which must be rejected before O_RDONLY is allowed.
    let pinned = match openat2_file(
        cache.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    let path = cache.path.join(name.to_string_lossy().as_ref());
    require_exact_cachedir_tag_metadata(&pinned, &path, CACHEDIR_TAG_MODE)?;
    let identity = cachedir_tag_identity(&pinned)?;

    let file = openat2_file(
        cache.file.as_raw_fd(),
        name,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_same_cachedir_tag(identity, cachedir_tag_identity(&file)?)?;
    require_exact_cachedir_tag_metadata(&file, &path, CACHEDIR_TAG_MODE)?;
    Ok(Some(file))
}

fn require_safe_cachedir_tag_temporary(file: &std::fs::File, path: &Path) -> io::Result<u32> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    let recoverable_mode = mode & !CACHEDIR_TAG_TEMPORARY_MODE == 0 || mode == CACHEDIR_TAG_MODE;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || !recoverable_mode
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cache-tag temporary is not one safely recoverable regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(mode)
}

fn require_exact_cachedir_tag(file: &mut std::fs::File, path: &Path, expected_mode: u32) -> io::Result<()> {
    require_exact_cachedir_tag_metadata(file, path, expected_mode)?;

    file.seek(SeekFrom::Start(0))?;
    let mut contents = Vec::with_capacity(CACHEDIR_TAG_CONTENTS.len() + 1);
    file.take(CACHEDIR_TAG_CONTENTS.len() as u64 + 1)
        .read_to_end(&mut contents)?;
    if contents != CACHEDIR_TAG_CONTENTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache tag has noncanonical contents: {}", path.display()),
        ));
    }
    Ok(())
}

fn require_exact_cachedir_tag_metadata(file: &std::fs::File, path: &Path, expected_mode: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || mode != expected_mode
        || metadata.len() != CACHEDIR_TAG_CONTENTS.len() as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cache tag is not one exact owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={}, bytes={})",
                path.display(),
                metadata.uid(),
                metadata.nlink(),
                metadata.len()
            ),
        ));
    }
    Ok(())
}

fn sync_exact_cachedir_tag(file: &mut std::fs::File, path: &Path) -> io::Result<()> {
    require_exact_cachedir_tag(file, path, CACHEDIR_TAG_MODE)?;
    file.sync_all()
}

fn cachedir_tag_identity(file: &std::fs::File) -> io::Result<CachedirTagIdentity> {
    let metadata = file.metadata()?;
    Ok(CachedirTagIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn require_same_cachedir_tag(expected: CachedirTagIdentity, actual: CachedirTagIdentity) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::other("cache-tag inode changed during atomic publication"))
    }
}

fn cleanup_cachedir_tag_identity(cache: &ControlledDirectory, expected: CachedirTagIdentity) -> io::Result<()> {
    let named = openat2_file(
        cache.file.as_raw_fd(),
        CACHEDIR_TAG_TEMPORARY_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_same_cachedir_tag(expected, cachedir_tag_identity(&named)?)?;
    unlinkat_name(cache.file.as_raw_fd(), CACHEDIR_TAG_TEMPORARY_NAME)?;
    cache.file.sync_all()
}

fn cleanup_cachedir_tag_after_failure(
    cache: &ControlledDirectory,
    identity: CachedirTagIdentity,
    source: io::Error,
) -> io::Error {
    match cleanup_cachedir_tag_identity(cache, identity) {
        Ok(()) => source,
        Err(cleanup) => io::Error::new(
            source.kind(),
            format!("{source}; retained cache-tag temporary cleanup also failed: {cleanup}"),
        ),
    }
}

fn renameat2_noreplace(directory: RawFd, from: &CStr, to: &CStr) -> io::Result<()> {
    loop {
        // SAFETY: the directory and both fixed single-component names remain
        // live. RENAME_NOREPLACE either publishes the retained inode or does
        // not change either name.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_renameat2,
                directory,
                from.as_ptr(),
                directory,
                to.as_ptr(),
                nix::libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn unlinkat_name(directory: RawFd, name: &CStr) -> io::Result<()> {
    loop {
        // SAFETY: the directory and single-component name remain live. flags
        // zero unlinks a non-directory without following its final component.
        if unsafe { nix::libc::unlinkat(directory, name.as_ptr(), 0) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn flock_exclusive(file: &std::fs::File) -> io::Result<()> {
    loop {
        // SAFETY: flock operates on the live temporary-file descriptor.
        if unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

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
    #[error("load authored Gluon system intent")]
    LoadSystemModel(#[from] system_model::LoadError),
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::{
            ffi::OsStrExt as _,
            fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _, symlink},
        },
        process::Command,
    };

    use crate::{Provider, test_support::private_installation_tempdir};

    use super::*;

    #[test]
    fn open_loads_only_the_canonical_authored_system_intent() {
        let temporary = private_installation_tempdir();
        let path = system_model::intent_path(temporary.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let authored = r#"let cast = import! cast.system.v1
{
    packages = ["alpha"],
    .. cast.system
}
"#;
        fs::write(&path, authored).unwrap();

        let installation = Installation::open(temporary.path(), None).unwrap();
        let intent = installation.system_model.as_ref().unwrap();

        assert_eq!(installation.system_intent_path(), path);
        assert_eq!(intent.authored_source(), authored);
        assert!(intent.packages.contains(&Provider::package_name("alpha")));
        assert!(!system_model::snapshot_path(temporary.path()).exists());
    }

    #[test]
    fn frozen_open_skips_active_state_and_invalid_system_intent() {
        let temporary = private_installation_tempdir();
        let intent_path = system_model::intent_path(temporary.path());
        fs::create_dir_all(intent_path.parent().unwrap()).unwrap();
        fs::write(&intent_path, b"invalid Gluon that normal open must reject").unwrap();
        fs::create_dir_all(temporary.path().join("usr")).unwrap();
        fs::write(temporary.path().join("usr/.stateID"), b"73").unwrap();

        let frozen = Installation::open_frozen(temporary.path(), None).unwrap();
        assert!(frozen.active_state.is_none());
        assert!(frozen.system_model.is_none());
        assert!(frozen.is_frozen_cache());
        drop(frozen);

        assert!(matches!(
            Installation::open(temporary.path(), None),
            Err(Error::LoadSystemModel(_))
        ));
    }

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn install_test_default_acl(path: &Path) -> io::Result<()> {
        // Linux POSIX ACL xattr encoding: version 2 followed by the canonical
        // user::rwx, group::r-x, and other::r-x default entries. A default ACL
        // does not appear in this directory's st_mode but would be inherited
        // by later children if installation provisioning admitted it.
        const ACL: [u8; 28] = [
            0x02, 0x00, 0x00, 0x00, // version
            0x01, 0x00, 0x07, 0x00, 0xff, 0xff, 0xff, 0xff, // user object
            0x04, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // group object
            0x20, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // other
        ];
        let directory = std::fs::File::open(path)?;
        // SAFETY: the directory, static name, and complete ACL byte array
        // remain live for the syscall.
        let result = unsafe {
            nix::libc::fsetxattr(
                directory.as_raw_fd(),
                POSIX_DEFAULT_ACL_XATTR.as_ptr(),
                ACL.as_ptr().cast(),
                ACL.len(),
                0,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn prepare_cast_parent(root: &Path) -> PathBuf {
        let cast = root.join(".cast");
        std::fs::create_dir(&cast).unwrap();
        std::fs::set_permissions(&cast, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
        cast
    }

    fn prepare_cache_parent(root: &Path) -> PathBuf {
        let cast = prepare_cast_parent(root);
        let cache = cast.join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
        cache
    }

    #[test]
    fn newly_created_capability_roots_have_exact_private_mode() {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();

        assert_eq!(mode(&temporary.path().join(".cast")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.cache_path("")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.assets_path("")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.state_quarantine_dir()), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&temporary.path().join(".cast/.cast-lockfile")), LOCKFILE_MODE);
        let tag = installation.cache_path("CACHEDIR.TAG");
        assert_eq!(mode(&tag), CACHEDIR_TAG_MODE);
        assert_eq!(std::fs::read(tag).unwrap(), CACHEDIR_TAG_CONTENTS);
    }

    #[test]
    fn safe_0555_installation_root_opens_read_only_without_provisioning() {
        let temporary = private_installation_tempdir();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let installation = Installation::open_frozen(temporary.path(), None).unwrap();

        assert!(installation.read_only());
        assert!(!temporary.path().join(".cast").exists());
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn installation_root_owner_and_write_policy_is_explicit() {
        let path = Path::new("/policy-test");
        assert!(require_installation_root_policy(true, 0, 0o555, 1000, path).is_ok());
        assert!(require_installation_root_policy(true, 1000, 0o755, 1000, path).is_ok());
        assert_eq!(
            require_installation_root_policy(true, 1001, 0o555, 1000, path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(classify_installation_root_access(0, 0o755, 1000), Mutability::ReadOnly);
        assert_eq!(
            classify_installation_root_access(1000, 0o555, 1000),
            Mutability::ReadOnly
        );
        assert_eq!(
            classify_installation_root_access(1000, 0o755, 1000),
            Mutability::ReadWrite
        );
    }

    #[test]
    fn installation_root_default_acl_is_rejected_before_provisioning() {
        let temporary = private_installation_tempdir();
        match install_test_default_acl(temporary.path()) {
            Ok(()) => {}
            Err(source) if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) => return,
            Err(source) => panic!("install test default ACL: {source}"),
        }
        assert_eq!(mode(temporary.path()), PRIVATE_DIRECTORY_MODE);

        let error = Installation::open(temporary.path(), None).unwrap_err();
        assert!(matches!(
            error,
            Error::ValidateRootDirectory { path, source }
                if path == temporary.path() && source.kind() == io::ErrorKind::PermissionDenied
        ));
        assert!(!temporary.path().join(".cast").exists());
    }

    #[test]
    fn existing_capability_default_acl_is_rejected_without_creating_children() {
        let temporary = private_installation_tempdir();
        let cast = prepare_cast_parent(temporary.path());
        match install_test_default_acl(&cast) {
            Ok(()) => {}
            Err(source) if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) => return,
            Err(source) => panic!("install test default ACL: {source}"),
        }

        let error = Installation::open(temporary.path(), None).unwrap_err();
        assert!(matches!(
            error,
            Error::PrepareDirectory { path, source }
                if path == cast && source.kind() == io::ErrorKind::PermissionDenied
        ));
        assert!(!cast.join("cache").exists());
    }

    #[test]
    fn named_installation_root_revalidation_detects_substitution() {
        let temporary = private_installation_tempdir();
        let root = temporary.path().join("root");
        let detached = temporary.path().join("detached-root");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
        let retained = open_installation_root_path(&root).unwrap();

        std::fs::rename(&root, &detached).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();

        assert!(require_named_installation_root(&root, &retained).is_err());
        assert_ne!(
            std::fs::metadata(&root).unwrap().ino(),
            std::fs::metadata(&detached).unwrap().ino()
        );
    }

    #[test]
    fn cachedir_tag_recovers_only_through_private_atomic_temporaries() {
        for (prefix, residue_mode) in [(0, 0o000), (17, 0o400), (CACHEDIR_TAG_CONTENTS.len() / 2, 0o600)] {
            let temporary = private_installation_tempdir();
            let cache = prepare_cache_parent(temporary.path());
            let residue = cache.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
            std::fs::write(&residue, &CACHEDIR_TAG_CONTENTS[..prefix]).unwrap();
            std::fs::set_permissions(&residue, std::fs::Permissions::from_mode(residue_mode)).unwrap();

            let installation = Installation::open(temporary.path(), None).unwrap();

            let canonical = installation.cache_path("CACHEDIR.TAG");
            assert_eq!(std::fs::read(canonical).unwrap(), CACHEDIR_TAG_CONTENTS);
            assert_eq!(mode(&installation.cache_path("CACHEDIR.TAG")), CACHEDIR_TAG_MODE);
            assert!(!residue.exists());
        }
    }

    #[test]
    fn complete_fsynced_cachedir_temporary_is_published_without_rewriting() {
        let temporary = private_installation_tempdir();
        let cache = prepare_cache_parent(temporary.path());
        let residue = cache.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
        std::fs::write(&residue, CACHEDIR_TAG_CONTENTS).unwrap();
        std::fs::set_permissions(&residue, std::fs::Permissions::from_mode(CACHEDIR_TAG_MODE)).unwrap();
        std::fs::File::open(&residue).unwrap().sync_all().unwrap();
        let inode = std::fs::metadata(&residue).unwrap().ino();

        let installation = Installation::open(temporary.path(), None).unwrap();
        let canonical = installation.cache_path("CACHEDIR.TAG");

        assert_eq!(std::fs::metadata(&canonical).unwrap().ino(), inode);
        assert_eq!(std::fs::read(canonical).unwrap(), CACHEDIR_TAG_CONTENTS);
        assert!(!residue.exists());
    }

    #[test]
    fn corrupt_canonical_cachedir_tags_fail_unchanged() {
        for contents in [
            b"not a cache tag".to_vec(),
            vec![b'x'; CACHEDIR_TAG_CONTENTS.len()],
            [CACHEDIR_TAG_CONTENTS, b"extra"].concat(),
        ] {
            let temporary = private_installation_tempdir();
            let cache = prepare_cache_parent(temporary.path());
            let canonical = cache.join("CACHEDIR.TAG");
            std::fs::write(&canonical, &contents).unwrap();
            std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(CACHEDIR_TAG_MODE)).unwrap();
            let original = std::fs::read(&canonical).unwrap();

            assert!(matches!(
                Installation::open(temporary.path(), None),
                Err(Error::PrepareCachedirTag { path, .. }) if path == canonical
            ));
            assert_eq!(std::fs::read(&canonical).unwrap(), original);
            assert!(
                !cache
                    .join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref())
                    .exists()
            );
        }
    }

    #[test]
    fn special_canonical_cachedir_tags_fail_before_data_open_and_remain_unchanged() {
        for kind in ["fifo", "symlink", "directory"] {
            let temporary = private_installation_tempdir();
            let cache = prepare_cache_parent(temporary.path());
            let canonical = cache.join("CACHEDIR.TAG");
            match kind {
                "fifo" => {
                    let encoded = CString::new(canonical.as_os_str().as_bytes()).unwrap();
                    // SAFETY: the path is NUL-terminated and names a missing
                    // entry inside the private test directory.
                    assert_eq!(unsafe { nix::libc::mkfifo(encoded.as_ptr(), CACHEDIR_TAG_MODE) }, 0);
                    std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(CACHEDIR_TAG_MODE)).unwrap();
                }
                "symlink" => {
                    std::fs::write(cache.join("target"), CACHEDIR_TAG_CONTENTS).unwrap();
                    symlink("target", &canonical).unwrap();
                }
                "directory" => {
                    std::fs::create_dir(&canonical).unwrap();
                }
                _ => unreachable!(),
            }
            let before = std::fs::symlink_metadata(&canonical).unwrap();

            assert!(matches!(
                Installation::open(temporary.path(), None),
                Err(Error::PrepareCachedirTag { path, .. }) if path == canonical
            ));

            let after = std::fs::symlink_metadata(&canonical).unwrap();
            assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()), "{kind}");
            assert_eq!(after.file_type().is_fifo(), before.file_type().is_fifo(), "{kind}");
            assert_eq!(
                after.file_type().is_symlink(),
                before.file_type().is_symlink(),
                "{kind}"
            );
            assert_eq!(after.file_type().is_dir(), before.file_type().is_dir(), "{kind}");
            if kind == "symlink" {
                assert_eq!(std::fs::read_link(&canonical).unwrap(), Path::new("target"));
            }
            assert!(
                !cache
                    .join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref())
                    .exists()
            );
        }
    }

    #[test]
    fn unsafe_cachedir_temporary_evidence_is_never_repaired_or_removed() {
        let temporary = private_installation_tempdir();
        let cache = prepare_cache_parent(temporary.path());
        let residue = cache.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
        std::fs::write(&residue, b"partial").unwrap();
        std::fs::set_permissions(&residue, std::fs::Permissions::from_mode(0o660)).unwrap();
        let original = std::fs::read(&residue).unwrap();

        assert!(matches!(
            Installation::open(temporary.path(), None),
            Err(Error::PrepareCachedirTag { .. })
        ));
        assert_eq!(mode(&residue), 0o660);
        assert_eq!(std::fs::read(&residue).unwrap(), original);
        assert!(!cache.join("CACHEDIR.TAG").exists());
    }

    #[test]
    fn hardlinked_cachedir_temporary_evidence_fails_unchanged() {
        let temporary = private_installation_tempdir();
        let cache = prepare_cache_parent(temporary.path());
        let residue = cache.join(CACHEDIR_TAG_TEMPORARY_NAME.to_string_lossy().as_ref());
        let second = cache.join("residue-link");
        std::fs::write(&residue, b"partial").unwrap();
        std::fs::set_permissions(&residue, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&residue, &second).unwrap();

        assert!(matches!(
            Installation::open(temporary.path(), None),
            Err(Error::PrepareCachedirTag { .. })
        ));
        assert_eq!(std::fs::metadata(&residue).unwrap().nlink(), 2);
        assert_eq!(std::fs::read(&residue).unwrap(), b"partial");
        assert_eq!(std::fs::read(&second).unwrap(), b"partial");
        assert!(!cache.join("CACHEDIR.TAG").exists());
    }

    #[test]
    fn umask_0777_cannot_strand_new_capability_roots() {
        const CHILD: &str = "CAST_INSTALLATION_UMASK_TEST_CHILD";
        const TEST: &str = "installation::tests::umask_0777_cannot_strand_new_capability_roots";

        if let Some(root) = std::env::var_os(CHILD) {
            // umask is process-global, so mutate it only in the isolated test
            // process selected by the parent branch below.
            // SAFETY: this child runs one exact test and exits immediately.
            unsafe { nix::libc::umask(0o777) };
            let installation = Installation::open(PathBuf::from(root), None).unwrap();
            assert_eq!(mode(&installation.root.join(".cast")), PRIVATE_DIRECTORY_MODE);
            assert_eq!(mode(&installation.cache_path("")), PRIVATE_DIRECTORY_MODE);
            assert_eq!(mode(&installation.assets_path("")), PRIVATE_DIRECTORY_MODE);
            assert_eq!(mode(&installation.state_quarantine_dir()), PRIVATE_DIRECTORY_MODE);
            for path in [
                installation.root.join(".cast/db"),
                installation.root.join(".cast/repo"),
                installation.root.join(".cast/root"),
                installation.root.join(".cast/root/staging"),
                installation.root.join(".cast/root/isolation"),
            ] {
                assert_eq!(mode(&path), PRIVATE_DIRECTORY_MODE, "{}", path.display());
            }
            return;
        }

        let temporary = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            temporary.path(),
            std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )
        .unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .arg(TEST)
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD, temporary.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "hostile-umask child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn restrictive_owner_only_crash_residue_is_recovered_exactly() {
        let temporary = private_installation_tempdir();
        let cast = temporary.path().join(".cast");
        let cache = cast.join("cache");
        let assets = cast.join("assets");
        let quarantine = cast.join("quarantine");
        for directory in [&cast, &cache, &assets, &quarantine] {
            std::fs::create_dir(directory).unwrap();
        }
        for (directory, residue) in [(&cache, 0o000), (&assets, 0o400), (&quarantine, 0o500), (&cast, 0o600)] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(residue)).unwrap();
        }

        let installation = Installation::open(temporary.path(), None).unwrap();

        assert_eq!(mode(&installation.root.join(".cast")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.cache_path("")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.assets_path("")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.state_quarantine_dir()), PRIVATE_DIRECTORY_MODE);
    }

    #[test]
    fn unsafe_preexisting_cast_root_is_unchanged_and_blocks_children() {
        let temporary = private_installation_tempdir();
        let cast = temporary.path().join(".cast");
        std::fs::create_dir(&cast).unwrap();
        std::fs::set_permissions(&cast, std::fs::Permissions::from_mode(0o770)).unwrap();

        let error = Installation::open(temporary.path(), None).unwrap_err();
        assert!(matches!(
            error,
            Error::PrepareDirectory { path, source }
                if path == cast && source.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(mode(&cast), 0o770);
        assert!(!cast.join("cache").exists());
    }

    #[test]
    fn installation_lockfile_symlink_is_rejected_without_touching_target() {
        let temporary = private_installation_tempdir();
        let cast = prepare_cast_parent(temporary.path());
        let target = temporary.path().join("external-lock-target");
        std::fs::write(&target, b"evidence").unwrap();
        let lockfile = cast.join(".cast-lockfile");
        symlink(&target, &lockfile).unwrap();

        let error = Installation::open(temporary.path(), None).unwrap_err();
        assert!(matches!(error, Error::PrepareLockfile { path, .. } if path == lockfile));
        assert_eq!(std::fs::read(&target).unwrap(), b"evidence");
        assert!(std::fs::symlink_metadata(&lockfile).unwrap().file_type().is_symlink());
    }

    #[test]
    fn installation_lockfile_requires_one_safe_inode_and_recovers_private_residue() {
        let temporary = private_installation_tempdir();
        let cast = prepare_cast_parent(temporary.path());
        let lockfile = cast.join(".cast-lockfile");
        let second = cast.join("second-lock-link");
        std::fs::write(&lockfile, b"").unwrap();
        std::fs::set_permissions(&lockfile, std::fs::Permissions::from_mode(LOCKFILE_MODE)).unwrap();
        std::fs::hard_link(&lockfile, &second).unwrap();

        let error = Installation::open(temporary.path(), None).unwrap_err();
        assert!(matches!(error, Error::PrepareLockfile { path, .. } if path == lockfile));
        assert_eq!(std::fs::metadata(&lockfile).unwrap().nlink(), 2);

        std::fs::remove_file(&second).unwrap();
        std::fs::set_permissions(&lockfile, std::fs::Permissions::from_mode(0o000)).unwrap();
        let installation = Installation::open(temporary.path(), None).unwrap();
        assert_eq!(mode(&lockfile), LOCKFILE_MODE);
        drop(installation);

        std::fs::set_permissions(&lockfile, std::fs::Permissions::from_mode(0o644)).unwrap();
        let installation = Installation::open(temporary.path(), None).unwrap();
        assert_eq!(mode(&lockfile), 0o644);
        drop(installation);
    }

    #[test]
    fn unsafe_installation_root_is_rejected_before_cast_creation() {
        for unsafe_mode in [0o775, 0o777] {
            let temporary = private_installation_tempdir();
            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(unsafe_mode)).unwrap();

            let error = Installation::open(temporary.path(), None).unwrap_err();
            assert!(matches!(
                error,
                Error::ValidateRootDirectory { path, source }
                    if path == temporary.path() && source.kind() == io::ErrorKind::PermissionDenied
            ));
            assert_eq!(mode(temporary.path()), unsafe_mode);
            assert!(!temporary.path().join(".cast").exists());
        }
    }

    #[test]
    fn existing_group_writable_cache_root_is_rejected_without_chmod_laundering() {
        let temporary = private_installation_tempdir();
        let cast = prepare_cast_parent(temporary.path());
        let cache = cast.join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o775)).unwrap();

        let error = Installation::open(temporary.path(), None).unwrap_err();
        assert!(matches!(
            error,
            Error::PrepareDirectory { path, source }
                if path == cache && source.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(mode(&cache), 0o775);
    }

    #[test]
    fn existing_group_writable_state_quarantine_is_rejected_without_chmod_laundering() {
        let temporary = private_installation_tempdir();
        let cast = prepare_cast_parent(temporary.path());
        for directory in ["cache", "assets"] {
            let path = cast.join(directory);
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
        }
        let quarantine = cast.join("quarantine");
        std::fs::create_dir(&quarantine).unwrap();
        std::fs::set_permissions(&quarantine, std::fs::Permissions::from_mode(0o775)).unwrap();

        let error = Installation::open(temporary.path(), None).unwrap_err();
        assert!(matches!(
            error,
            Error::PrepareDirectory { path, source }
                if path == quarantine && source.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(mode(&quarantine), 0o775);
    }

    #[test]
    fn existing_readonly_shared_cache_root_remains_compatible() {
        let temporary = private_installation_tempdir();
        let cast = prepare_cast_parent(temporary.path());
        let cache = cast.join("cache");
        let assets = cast.join("assets");
        let quarantine = cast.join("quarantine");
        std::fs::set_permissions(&cast, std::fs::Permissions::from_mode(0o750)).unwrap();
        for (directory, existing_mode) in [(&cache, 0o755), (&assets, 0o750), (&quarantine, 0o711)] {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(existing_mode)).unwrap();
        }

        Installation::open(temporary.path(), None).unwrap();
        assert_eq!(mode(&cast), 0o750);
        assert_eq!(mode(&cache), 0o755);
        assert_eq!(mode(&assets), 0o750);
        assert_eq!(mode(&quarantine), 0o711);
    }

    #[test]
    fn cache_root_symlink_is_rejected_without_touching_its_target() {
        let temporary = private_installation_tempdir();
        let cast = prepare_cast_parent(temporary.path());
        let target = temporary.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cache = cast.join("cache");
        symlink(&target, &cache).unwrap();

        assert!(matches!(
            Installation::open(temporary.path(), None),
            Err(Error::PrepareDirectory { path, .. }) if path == cache
        ));
        assert_eq!(mode(&target), 0o755);
        assert!(std::fs::symlink_metadata(cache).unwrap().file_type().is_symlink());
    }

    #[test]
    fn cache_root_wrong_kind_is_rejected_without_replacement() {
        let temporary = private_installation_tempdir();
        let cast = prepare_cast_parent(temporary.path());
        let cache = cast.join("cache");
        std::fs::write(&cache, b"not a directory").unwrap();

        assert!(matches!(
            Installation::open(temporary.path(), None),
            Err(Error::PrepareDirectory { path, .. }) if path == cache
        ));
        assert_eq!(std::fs::read(cache).unwrap(), b"not a directory");
    }

    #[test]
    fn custom_cache_root_uses_the_same_owner_and_mode_policy() {
        let temporary = private_installation_tempdir();
        let custom = temporary.path().join("custom-cache");
        std::fs::create_dir(&custom).unwrap();
        std::fs::set_permissions(&custom, std::fs::Permissions::from_mode(0o775)).unwrap();

        let error = Installation::open(temporary.path(), Some(custom.clone())).unwrap_err();
        assert!(matches!(
            error,
            Error::ValidateCacheDirectory { path, source }
                if path == custom && source.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(mode(&custom), 0o775);
    }

    #[test]
    fn custom_cache_symlink_is_rejected() {
        let temporary = private_installation_tempdir();
        let custom_target = temporary.path().join("custom-target");
        let custom_link = temporary.path().join("custom-link");
        std::fs::create_dir(&custom_target).unwrap();
        std::fs::set_permissions(&custom_target, std::fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&custom_target, &custom_link).unwrap();

        assert!(matches!(
            Installation::open(temporary.path(), Some(custom_link.clone())),
            Err(Error::ValidateCacheDirectory { path, .. }) if path == custom_link
        ));
    }

    #[test]
    fn directory_policy_rejects_a_wrong_owner() {
        let temporary = tempfile::tempdir().unwrap();
        let metadata = std::fs::metadata(temporary.path()).unwrap();
        let wrong_owner = metadata.uid().wrapping_add(1);

        let error = require_controlled_directory_metadata(&metadata, temporary.path(), wrong_owner).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
