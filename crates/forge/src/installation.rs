// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Encapsulation of a target installation filesystem

use std::{
    ffi::{CStr, CString, OsStr},
    io::{self, Write as _},
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
use nix::unistd::{AccessFlags, Uid, access};
use thiserror::Error;
use tui::Styled;

use crate::{
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
    /// and determine the mutability per the current user identity
    /// and ACL permissions.
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

        if let Some(dir) = &cache_dir
            && (!dir.exists() || !dir.is_dir())
        {
            return Err(Error::CacheInvalid);
        }

        if let Some(dir) = &cache_dir {
            validate_capability_root(dir).map_err(|source| Error::ValidateCacheDirectory {
                path: dir.clone(),
                source,
            })?;
        }

        // Root? Always RW. Otherwise, check access for W
        let mutability = if Uid::effective().is_root() || access(&root, AccessFlags::W_OK).is_ok() {
            Mutability::ReadWrite
        } else {
            Mutability::ReadOnly
        };
        if matches!(mutability, Mutability::ReadWrite) {
            ensure_dirs_exist(&root)?;
        }

        trace!("Mutability: {mutability}");
        trace!("Root dir: {root:?}");

        // Get exclusive access to work within these directories
        let _locks = if matches!(mutability, Mutability::ReadWrite) {
            acquire_locks(&root.join(".cast"), cache_dir.as_deref())?
        } else {
            vec![]
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

/// Blocks until lockfiles can be obtained for the
/// root Cast path and if provided, the custom
/// cache path
///
/// Locks are held until dropped
pub fn acquire_locks(cast_path: &Path, cache_dir: Option<&Path>) -> Result<Vec<lockfile::Lock>, Error> {
    let mut locks = vec![];

    locks.push(lockfile::acquire(
        cast_path.join(".cast-lockfile"),
        format!("{} another process is using the Cast root", "Blocking".yellow().bold()),
    )?);

    if let Some(path) = cache_dir {
        locks.push(lockfile::acquire(
            path.join(".cast-lockfile"),
            format!("{} another process is using the cache dir", "Blocking".yellow().bold()),
        )?);
    }

    Ok(locks)
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
const CACHEDIR_TAG_MODE: u32 = 0o644;
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
fn ensure_dirs_exist(root: &Path) -> Result<(), Error> {
    let root_directory = open_directory_path(root).map_err(|source| Error::PrepareDirectory {
        path: root.to_owned(),
        source,
    })?;
    let cast_path = root.join(".cast");
    let cast = ensure_controlled_child(&root_directory, OsStr::new(".cast"), &cast_path).map_err(|source| {
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

    // These are not content-addressed capability roots. Keep their historical
    // layout while making creation failures visible to the caller.
    for path in [
        cast_path.join("db"),
        cast_path.join("repo"),
        cast_path.join("root").join("staging"),
        cast_path.join("root").join("isolation"),
    ] {
        fs::create_dir_all(&path).map_err(|source| Error::PrepareDirectory { path, source })?;
    }

    ensure_cachedir_tag(&cache).map_err(|source| Error::PrepareCachedirTag {
        path: cache_path.join("CACHEDIR.TAG"),
        source,
    })?;
    Ok(())
}

fn validate_capability_root(path: &Path) -> io::Result<()> {
    let directory = open_directory_path(path)?;
    require_controlled_directory(&directory, path)
}

fn ensure_controlled_child(parent: &std::fs::File, name: &OsStr, path: &Path) -> io::Result<ControlledDirectory> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory name contains NUL"))?;
    // SAFETY: `parent` and the single NUL-terminated component remain live.
    let created = if unsafe { nix::libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), PRIVATE_DIRECTORY_MODE) } == 0 {
        true
    } else {
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::AlreadyExists {
            return Err(source);
        }
        false
    };

    let pinned = openat2_file(
        parent.as_raw_fd(),
        name.as_c_str(),
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    if created {
        // Only an inode created by this call may be normalized. An unsafe
        // pre-existing entry is evidence and must fail unchanged.
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
    require_same_directory(&pinned, &directory, path)?;
    Ok(ControlledDirectory {
        file: directory,
        path: path.to_owned(),
    })
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

fn openat2_file(dirfd: RawFd, path: &CStr, flags: i32, mode: u32, resolve: u64) -> io::Result<std::fs::File> {
    // SAFETY: zero is valid for every public `open_how` field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: the descriptor, C string, and open_how remain live. Success
    // returns one fresh descriptor owned below.
    let descriptor = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
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

fn chmod_path_descriptor(file: &std::fs::File, mode: u32) -> io::Result<()> {
    // fchmod rejects O_PATH. Linux fchmodat2 with AT_EMPTY_PATH changes the
    // exact retained inode without reopening an attacker-controlled path.
    // SAFETY: `file` and the static empty C string remain live.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_fchmodat2,
            file.as_raw_fd(),
            c"".as_ptr(),
            mode,
            nix::libc::AT_EMPTY_PATH,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Ensure we install a cachedir tag to prevent backup tools from archiving
/// the contents of this tree. Creation is exclusive and descriptor-relative;
/// a pre-existing symlink or non-regular entry is rejected.
fn ensure_cachedir_tag(cache: &ControlledDirectory) -> io::Result<()> {
    let name = c"CACHEDIR.TAG";
    let mut flags = nix::libc::O_WRONLY
        | nix::libc::O_CLOEXEC
        | nix::libc::O_NOFOLLOW
        | nix::libc::O_NONBLOCK
        | nix::libc::O_CREAT
        | nix::libc::O_EXCL;
    let (mut file, created) = match openat2_file(
        cache.file.as_raw_fd(),
        name,
        flags,
        CACHEDIR_TAG_MODE,
        controlled_resolution(),
    ) {
        Ok(file) => (file, true),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            flags &= !(nix::libc::O_CREAT | nix::libc::O_EXCL);
            flags &= !nix::libc::O_WRONLY;
            flags |= nix::libc::O_RDONLY;
            (
                openat2_file(cache.file.as_raw_fd(), name, flags, 0, controlled_resolution())?,
                false,
            )
        }
        Err(source) => return Err(source),
    };

    if created {
        file.set_permissions(std::fs::Permissions::from_mode(CACHEDIR_TAG_MODE))?;
        if let Err(source) = file.write_all(CACHEDIR_TAG_CONTENTS).and_then(|()| file.sync_all()) {
            // SAFETY: the parent and static single component remain live.
            let _ = unsafe { nix::libc::unlinkat(cache.file.as_raw_fd(), name.as_ptr(), 0) };
            return Err(source);
        }
        cache.file.sync_all()?;
    }
    require_safe_cachedir_tag(&file, &cache.path.join("CACHEDIR.TAG"))
}

fn require_safe_cachedir_tag(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || mode & 0o7000 != 0
        || mode & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cache tag is not one safe owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(())
}

/// Errors specific to a target installation filesystem
#[derive(Debug, Error)]
pub enum Error {
    #[error("Root is invalid")]
    RootInvalid,
    #[error("Cache dir is invalid")]
    CacheInvalid,
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
    #[error("acquiring lockfile")]
    Lockfile(#[from] lockfile::Error),
    #[error("load authored Gluon system intent")]
    LoadSystemModel(#[from] system_model::LoadError),
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::{PermissionsExt as _, symlink},
        process::Command,
    };

    use crate::Provider;

    use super::*;

    #[test]
    fn open_loads_only_the_canonical_authored_system_intent() {
        let temporary = tempfile::tempdir().unwrap();
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
        let temporary = tempfile::tempdir().unwrap();
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

    fn prepare_cast_parent(root: &Path) -> PathBuf {
        let cast = root.join(".cast");
        std::fs::create_dir(&cast).unwrap();
        std::fs::set_permissions(&cast, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
        cast
    }

    #[test]
    fn newly_created_capability_roots_have_exact_private_mode() {
        let temporary = tempfile::tempdir().unwrap();
        let installation = Installation::open(temporary.path(), None).unwrap();

        assert_eq!(mode(&temporary.path().join(".cast")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.cache_path("")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.assets_path("")), PRIVATE_DIRECTORY_MODE);
        assert_eq!(mode(&installation.state_quarantine_dir()), PRIVATE_DIRECTORY_MODE);
    }

    #[test]
    fn hostile_umask_cannot_weaken_new_capability_roots() {
        const CHILD: &str = "CAST_INSTALLATION_UMASK_TEST_CHILD";
        const TEST: &str = "installation::tests::hostile_umask_cannot_weaken_new_capability_roots";

        if let Some(root) = std::env::var_os(CHILD) {
            // umask is process-global, so mutate it only in the isolated test
            // process selected by the parent branch below.
            // SAFETY: this child runs one exact test and exits immediately.
            unsafe { nix::libc::umask(0o002) };
            let installation = Installation::open(PathBuf::from(root), None).unwrap();
            assert_eq!(mode(&installation.root.join(".cast")), PRIVATE_DIRECTORY_MODE);
            assert_eq!(mode(&installation.cache_path("")), PRIVATE_DIRECTORY_MODE);
            assert_eq!(mode(&installation.assets_path("")), PRIVATE_DIRECTORY_MODE);
            assert_eq!(mode(&installation.state_quarantine_dir()), PRIVATE_DIRECTORY_MODE);
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
    fn existing_group_writable_cache_root_is_rejected_without_chmod_laundering() {
        let temporary = tempfile::tempdir().unwrap();
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
        let temporary = tempfile::tempdir().unwrap();
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
        let temporary = tempfile::tempdir().unwrap();
        let cast = prepare_cast_parent(temporary.path());
        let cache = cast.join("cache");
        let assets = cast.join("assets");
        for directory in [&cache, &assets] {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        Installation::open(temporary.path(), None).unwrap();
        assert_eq!(mode(&cache), 0o755);
        assert_eq!(mode(&assets), 0o755);
    }

    #[test]
    fn cache_root_symlink_is_rejected_without_touching_its_target() {
        let temporary = tempfile::tempdir().unwrap();
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
        let temporary = tempfile::tempdir().unwrap();
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
        let temporary = tempfile::tempdir().unwrap();
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
        let temporary = tempfile::tempdir().unwrap();
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
