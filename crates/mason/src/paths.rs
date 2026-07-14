// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{CStr, CString, OsStr},
    fs::File as StdFile,
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(test)]
use forge::util;
#[cfg(test)]
use fs_err::File;
use nix::fcntl::{FlockArg, flock};
use stone_recipe::derivation::{BuilderLayout, DerivationPlan};

use crate::{Recipe, linux_fs::chmod_path_descriptor};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Id {
    Recipe(String),
    Derivation(String),
}

impl Id {
    fn recipe(recipe: &Recipe) -> Self {
        Self::Recipe(format!(
            "{}-{}-{}",
            recipe.declaration.meta.pname, recipe.declaration.meta.version, recipe.declaration.meta.release
        ))
    }

    fn derivation(plan: &DerivationPlan) -> Self {
        Self::Derivation(plan.derivation_id().to_string())
    }

    fn value(&self) -> &str {
        match self {
            Self::Recipe(value) | Self::Derivation(value) => value,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    id: Id,
    host_root: PathBuf,
    host_root_anchor: Arc<StdFile>,
    layout: BuilderLayout,
    recipe_dir: PathBuf,
    output_dir: PathBuf,
}

impl Paths {
    pub fn new(
        recipe: &Recipe,
        layout: BuilderLayout,
        host_root: impl Into<PathBuf>,
        output_dir: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        recipe
            .declaration
            .validate()
            .map_err(|error| invalid_binding(format!("invalid package identity before path creation: {error}")))?;
        let id = Id::recipe(recipe);

        let recipe_dir = recipe.path.parent().unwrap_or(&PathBuf::default()).canonicalize()?;

        let host_root = host_root.into().canonicalize()?;
        let host_root_anchor = Arc::new(open_directory_nofollow(&host_root)?);
        require_controlled_directory(&host_root_anchor, &host_root, false)?;

        let job = Self {
            id,
            host_root,
            host_root_anchor,
            layout,
            recipe_dir,
            output_dir: output_dir.into(),
        };

        for path in [
            job.rootfs().host,
            job.artefacts().host,
            job.build().host,
            job.ccache().host,
            job.gocache().host,
            job.gomodcache().host,
            job.cargocache().host,
            job.zigcache().host,
            job.sccache().host,
            job.upstreams().host,
        ] {
            job.prepare_private_host_directory(&path)?;
        }

        Ok(job)
    }

    /// Bind paths used by frozen execution to the exact validated derivation.
    ///
    /// `Paths::new` intentionally retains the recipe-keyed workspace used by
    /// legacy/chroot operation. The planner calls this only after validating a
    /// frozen plan, before constructing its runtime.
    pub fn bind_to_plan(&mut self, plan: &DerivationPlan) -> io::Result<()> {
        if self.layout != plan.layout {
            return Err(invalid_binding(
                "runtime paths do not use the frozen plan's builder layout".to_owned(),
            ));
        }
        let expected = Id::derivation(plan);
        match &self.id {
            Id::Recipe(_) => self.id = expected,
            current if current == &expected => {}
            Id::Derivation(current) => {
                return Err(invalid_binding(format!(
                    "runtime paths are already bound to derivation {current}, not {}",
                    plan.derivation_id()
                )));
            }
        }

        // The frozen root is an atomic publication destination.  It must stay
        // absent until Forge has completely materialized and normalized a
        // private sibling staging tree; pre-creating it would either force an
        // in-place rebuild or weaken publication into replacement semantics.
        // `Paths::new` has already created the trusted `root/` parent. The
        // derivation-scoped writable directories likewise remain absent until
        // frozen-sandbox preparation creates and pins them beneath the retained
        // workspace descriptor.
        self.prepare_private_host_directory(&self.execution_lock_dir())?;
        Ok(())
    }

    /// Require these paths to be bound to this exact plan.
    pub fn require_plan(&self, plan: &DerivationPlan) -> io::Result<()> {
        let expected = plan.derivation_id();
        match &self.id {
            Id::Derivation(current) if current == expected.as_str() => Ok(()),
            Id::Derivation(current) => Err(invalid_binding(format!(
                "runtime paths are bound to derivation {current}, not {expected}"
            ))),
            Id::Recipe(current) => Err(invalid_binding(format!(
                "recipe-keyed paths {current} are not bound to frozen derivation {expected}"
            ))),
        }
    }

    /// Stable host lock path shared by every execution of this derivation.
    pub fn execution_lock_path(&self, plan: &DerivationPlan) -> io::Result<PathBuf> {
        self.require_plan(plan)?;
        let leaf = execution_lock_leaf(plan.derivation_id().as_str())?;
        Ok(self.execution_lock_dir().join(OsStr::from_bytes(leaf.to_bytes())))
    }

    /// Acquire the process-level lock that serializes identical derivations.
    pub fn acquire_execution_lock(&self, plan: &DerivationPlan) -> io::Result<ExecutionLock> {
        let path = self.execution_lock_path(plan)?;
        let leaf = execution_lock_leaf(plan.derivation_id().as_str())?;

        // Keep one stable workspace inode locked for the guard's whole
        // lifetime. The per-derivation pathname can therefore be unlinked or
        // replaced only by a process which ignores this protocol; another
        // `Paths` acquisition still cannot return a second live guard until
        // this one is dropped. Reopen rather than dup the retained descriptor:
        // flock is attached to an open file description, so dup would let two
        // acquisitions from the same `Paths` instance share one lock.
        self.revalidate_host_root()?;
        let workspace_gate = open_directory_nofollow(&self.host_root)?;
        require_same_directory(&self.host_root_anchor, &workspace_gate, &self.host_root)?;
        lock_exclusive(&workspace_gate)?;
        self.revalidate_host_root()?;

        let lock_dir = self.prepare_private_host_directory(&self.execution_lock_dir())?;
        let root_metadata = self.host_root_anchor.metadata()?;
        require_same_device(&root_metadata, &lock_dir, &path)?;
        let lock_dir_identity = directory_identity(&lock_dir)?;

        let file = open_or_create_execution_lock_file(&lock_dir, &leaf)?;
        let file_identity = require_controlled_lock_file(&file, &root_metadata, &path)?;
        lock_exclusive(&file)?;
        if require_controlled_lock_file(&file, &root_metadata, &path)? != file_identity {
            return Err(io::Error::other(format!(
                "execution lock file changed while flocking it: {path:?}"
            )));
        }

        let reopened = open_execution_lock_file(&lock_dir, &leaf, false)?;
        if require_controlled_lock_file(&reopened, &root_metadata, &path)? != file_identity {
            return Err(io::Error::other(format!(
                "execution lock path was replaced while acquiring it: {path:?}"
            )));
        }
        self.revalidate_host_root()?;

        Ok(ExecutionLock {
            _workspace_gate: workspace_gate,
            lock_dir,
            file,
            workspace_identity: directory_identity(&self.host_root_anchor)?,
            lock_dir_identity,
            file_identity,
            path,
        })
    }

    pub fn require_execution_lock(&self, lock: &ExecutionLock, plan: &DerivationPlan) -> io::Result<()> {
        let expected = self.execution_lock_path(plan)?;
        if lock.path != expected {
            return Err(invalid_binding(format!(
                "execution guard {:?} does not lock frozen derivation path {expected:?}",
                lock.path
            )));
        }

        self.revalidate_host_root()?;
        let workspace_identity = directory_identity(&self.host_root_anchor)?;
        if lock.workspace_identity != workspace_identity
            || directory_identity(&lock._workspace_gate)? != workspace_identity
        {
            return Err(invalid_binding(format!(
                "execution guard does not lock workspace {:?}",
                self.host_root
            )));
        }

        let lock_dir = self.prepare_private_host_directory(&self.execution_lock_dir())?;
        if lock.lock_dir_identity != directory_identity(&lock.lock_dir)?
            || lock.lock_dir_identity != directory_identity(&lock_dir)?
        {
            return Err(io::Error::other(format!(
                "execution lock directory was replaced for {expected:?}"
            )));
        }

        let root_metadata = self.host_root_anchor.metadata()?;
        if require_controlled_lock_file(&lock.file, &root_metadata, &expected)? != lock.file_identity {
            return Err(io::Error::other(format!(
                "execution lock descriptor changed for {expected:?}"
            )));
        }
        let leaf = execution_lock_leaf(plan.derivation_id().as_str())?;
        let reopened = open_execution_lock_file(&lock_dir, &leaf, false)?;
        if require_controlled_lock_file(&reopened, &root_metadata, &expected)? != lock.file_identity {
            return Err(io::Error::other(format!(
                "execution lock path was replaced for {expected:?}"
            )));
        }
        self.revalidate_host_root()
    }

    fn execution_lock_dir(&self) -> PathBuf {
        self.host_root.join("locks")
    }

    pub(crate) fn workspace_path(&self) -> &Path {
        &self.host_root
    }

    /// Clone the workspace descriptor retained since path construction.
    ///
    /// The returned descriptor is authoritative. The pathname is reopened and
    /// compared first so a renamed or substituted cache root cannot silently
    /// redirect later frozen setup.
    pub(crate) fn frozen_workspace_anchor(&self) -> io::Result<(PathBuf, StdFile)> {
        self.revalidate_host_root()?;
        Ok((self.host_root.clone(), self.host_root_anchor.try_clone()?))
    }

    /// Create or reopen one host path beneath the retained workspace anchor.
    ///
    /// Every component is descriptor-relative, refuses links and mount
    /// crossings, and must remain owned and non-shared-writable. Existing
    /// unsafe directories fail closed; a safe leaf is normalized to exact
    /// mode 0700 before its descriptor is returned.
    pub(crate) fn prepare_private_host_directory(&self, path: &Path) -> io::Result<StdFile> {
        self.revalidate_host_root()?;
        let relative = private_host_relative(&self.host_root, path)?;
        let directory = ensure_private_directory_at(&self.host_root_anchor, &relative, path)?;
        self.revalidate_host_root()?;
        Ok(directory)
    }

    /// Atomically replace a derivation scratch directory with one empty,
    /// owner-private leaf and return its pinned descriptor.
    ///
    /// An old live tree is first detached to one deterministic quarantine name.
    /// Quarantine disposal is descriptor-rooted and bounded; no recursive host
    /// pathname operation can race creation of the new live leaf.
    pub(crate) fn prepare_fresh_private_host_directory(&self, path: &Path) -> io::Result<StdFile> {
        self.revalidate_host_root()?;
        let relative = private_host_relative(&self.host_root, path)?;
        let (parent, leaf) = private_parent_and_leaf(&self.host_root_anchor, &relative, path, true)?;
        let parent = parent.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("private host directory parent was not created: {path:?}"),
            )
        })?;
        let stale = stale_leaf_name(&leaf)?;
        let mut budget = PurgeBudget::new(&self.host_root_anchor)?;
        purge_named_entry(&parent, &stale, &mut budget, 0, path)?;

        if let Some(live) = open_controlled_named_directory(&parent, &leaf, path)? {
            let witness = directory_identity(&live)?;
            rename_noreplace(&parent, &leaf, &stale, path)?;
            let detached = open_controlled_named_directory(&parent, &stale, path)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("detached private host directory disappeared: {path:?}"),
                )
            })?;
            if directory_identity(&detached)? != witness {
                return Err(io::Error::other(format!(
                    "private host directory changed during atomic detach: {path:?}"
                )));
            }
        }

        create_private_leaf(&parent, &leaf, path)?;
        let live = open_controlled_named_directory(&parent, &leaf, path)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("fresh private host directory disappeared: {path:?}"),
            )
        })?;
        purge_named_entry(&parent, &stale, &mut budget, 0, path)?;
        self.revalidate_host_root()?;
        Ok(live)
    }

    /// Atomically detach and boundedly dispose one derivation scratch tree.
    pub(crate) fn remove_private_host_directory(&self, path: &Path) -> io::Result<()> {
        self.revalidate_host_root()?;
        let relative = private_host_relative(&self.host_root, path)?;
        let (parent, leaf) = private_parent_and_leaf(&self.host_root_anchor, &relative, path, false)?;
        let Some(parent) = parent else {
            return Ok(());
        };
        let stale = stale_leaf_name(&leaf)?;
        let mut budget = PurgeBudget::new(&self.host_root_anchor)?;
        purge_named_entry(&parent, &stale, &mut budget, 0, path)?;
        if let Some(live) = open_controlled_named_directory(&parent, &leaf, path)? {
            let witness = directory_identity(&live)?;
            rename_noreplace(&parent, &leaf, &stale, path)?;
            let detached = open_controlled_named_directory(&parent, &stale, path)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("detached private host directory disappeared: {path:?}"),
                )
            })?;
            if directory_identity(&detached)? != witness {
                return Err(io::Error::other(format!(
                    "private host directory changed during atomic detach: {path:?}"
                )));
            }
        }
        purge_named_entry(&parent, &stale, &mut budget, 0, path)?;
        self.revalidate_host_root()
    }

    fn revalidate_host_root(&self) -> io::Result<()> {
        require_controlled_directory(&self.host_root_anchor, &self.host_root, false)?;
        let reopened = open_directory_nofollow(&self.host_root)?;
        require_controlled_directory(&reopened, &self.host_root, false)?;
        require_same_directory(&self.host_root_anchor, &reopened, &self.host_root)
    }

    pub fn rootfs(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("root").join(self.id.value()),
            guest: "/".into(),
        }
    }

    pub fn artefacts(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("artefacts").join(self.id.value()),
            guest: self.layout.artifacts_dir.clone().into(),
        }
    }

    pub fn build(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("build").join(self.id.value()),
            guest: self.layout.build_dir.clone().into(),
        }
    }

    pub fn ccache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("ccache"),
            guest: self.layout.ccache_dir.clone().into(),
        }
    }

    pub fn gocache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("gocache"),
            guest: self.layout.go_cache_dir.clone().into(),
        }
    }

    pub fn gomodcache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("gomodcache"),
            guest: self.layout.go_mod_cache_dir.clone().into(),
        }
    }

    pub fn cargocache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("cargocache"),
            guest: self.layout.cargo_cache_dir.clone().into(),
        }
    }

    pub fn zigcache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("zigcache"),
            guest: self.layout.zig_cache_dir.clone().into(),
        }
    }

    pub fn sccache(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("sccache"),
            guest: self.layout.sccache_dir.clone().into(),
        }
    }

    /// Cache mapping isolated by frozen derivation identity.
    pub fn derivation_cache_host(&self, derivation_id: &str, name: &str) -> PathBuf {
        self.host_root.join("derivations").join(derivation_id).join(name)
    }

    pub fn upstreams(&self) -> Mapping {
        Mapping {
            host: self.host_root.join("upstreams"),
            guest: self.layout.source_dir.clone().into(),
        }
    }

    pub fn recipe(&self) -> Mapping {
        Mapping {
            host: self.recipe_dir.clone(),
            guest: self.layout.recipe_dir.clone().into(),
        }
    }

    pub fn install(&self) -> Mapping {
        let guest = PathBuf::from(&self.layout.install_dir);
        let host = self.guest_host_path(&Mapping {
            host: PathBuf::new(),
            guest: guest.clone(),
        });
        Mapping { host, guest }
    }

    /// For the provided [`Mapping`], return the guest
    /// path as it lives on the host fs
    ///
    /// Example:
    /// - host = "/var/cache/cast/build/root/test"
    /// - guest = "/sandbox/build"
    /// - guest_host_path = "/var/cache/cast/build/root/test/sandbox/build"
    pub fn guest_host_path(&self, mapping: &Mapping) -> PathBuf {
        let relative = mapping.guest.strip_prefix("/").unwrap_or(&mapping.guest);

        self.rootfs().host.join(relative)
    }

    /// Returns the output directory used for artefact syncing
    pub fn output_dir(&self) -> &PathBuf {
        &self.output_dir
    }

    pub fn layout(&self) -> &BuilderLayout {
        &self.layout
    }
}

pub struct Mapping {
    pub host: PathBuf,
    pub guest: PathBuf,
}

/// Held for the complete destructive setup/build/package/cleanup interval.
/// The kernel releases the flock when this guard is dropped.
#[derive(Debug)]
pub struct ExecutionLock {
    _workspace_gate: StdFile,
    lock_dir: StdFile,
    file: StdFile,
    workspace_identity: (u64, u64),
    lock_dir_identity: (u64, u64),
    file_identity: (u64, u64),
    path: PathBuf,
}

impl ExecutionLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[allow(deprecated)]
fn lock_exclusive(file: &impl AsRawFd) -> io::Result<()> {
    flock(file.as_raw_fd(), FlockArg::LockExclusive).map_err(errno_to_io)
}

fn errno_to_io(error: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

fn invalid_binding(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

const MAX_PRIVATE_HOST_PATH_BYTES: usize = 4095;
const MAX_PRIVATE_HOST_PATH_COMPONENTS: usize = 32;
const MAX_EXECUTION_LOCK_NAME_BYTES: usize = 255;
const EXECUTION_LOCK_SUFFIX: &[u8] = b".lock";

fn execution_lock_leaf(derivation_id: &str) -> io::Result<CString> {
    let id = derivation_id.as_bytes();
    let name_len = id
        .len()
        .checked_add(EXECUTION_LOCK_SUFFIX.len())
        .ok_or_else(|| invalid_binding("execution lock name length overflowed".to_owned()))?;
    if id.is_empty() || name_len > MAX_EXECUTION_LOCK_NAME_BYTES || id.iter().any(|byte| *byte == b'/' || *byte == 0) {
        return Err(invalid_binding(format!(
            "invalid derivation identity for execution lock ({} bytes)",
            id.len()
        )));
    }
    let mut name = Vec::with_capacity(name_len);
    name.extend_from_slice(id);
    name.extend_from_slice(EXECUTION_LOCK_SUFFIX);
    CString::new(name).map_err(|_| invalid_binding("execution lock name contains NUL".to_owned()))
}

fn open_or_create_execution_lock_file(parent: &StdFile, name: &CStr) -> io::Result<StdFile> {
    match open_execution_lock_file(parent, name, true) {
        Ok(file) => {
            // O_EXCL proves this call created the inode, so normalizing the
            // exact pinned descriptor cannot launder an unsafe pre-existing
            // entry. This also makes creation independent of the caller's
            // process-global umask.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            Ok(file)
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => open_execution_lock_file(parent, name, false),
        Err(source) => Err(source),
    }
}

fn open_execution_lock_file(parent: &StdFile, name: &CStr, create_exclusive: bool) -> io::Result<StdFile> {
    // O_NONBLOCK is required even though a valid lock is a regular file: a
    // hostile pre-existing FIFO must be rejected rather than hanging before
    // its type can be authenticated.
    let mut flags = nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK;
    if create_exclusive {
        flags |= nix::libc::O_CREAT | nix::libc::O_EXCL;
    }
    // SAFETY: an all-zero open_how is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = if create_exclusive { 0o600 } else { 0 };
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV;
    // SAFETY: parent, the single validated component, and open_how remain live
    // for the syscall.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(result)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {result}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    Ok(unsafe { StdFile::from_raw_fd(descriptor) })
}

fn require_controlled_lock_file(file: &StdFile, root: &std::fs::Metadata, path: &Path) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let owner = unsafe { nix::libc::geteuid() };
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner
        || metadata.dev() != root.dev()
        || mode != 0o600
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "execution lock is not one private regular file: {path:?} (uid={}, mode={mode:#06o}, links={})",
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok((metadata.dev(), metadata.ino()))
}

/// Establish one dedicated owner-private workspace root without trusting the
/// ambient umask or following a final symlink.
///
/// The nearest existing parent is canonicalized and authenticated first. Every
/// missing descendant is then created through that descriptor. Existing
/// shared-writable components fail before any chmod; only an already-safe leaf
/// or a directory created by this call is normalized to exact mode 0700.
#[cfg(test)]
pub(crate) fn prepare_private_workspace_root(path: &Path) -> io::Result<PathBuf> {
    prepare_private_workspace_root_pinned(path).map(|(path, _anchor)| path)
}

/// Establish and retain one exact owner-private workspace root.
///
/// Keeping the descriptor returned by this operation lets a later destructive
/// caller prove that the pathname still denotes the root selected here rather
/// than merely pinning whichever directory happens to occupy the name later.
pub(crate) fn prepare_private_workspace_root_pinned(path: &Path) -> io::Result<(PathBuf, StdFile)> {
    prepare_private_workspace_root_with_policy_pinned(path, WorkspaceRootLeafPolicy::NormalizeExisting)
}

/// Create a missing owner-private workspace root without changing an existing
/// final entry.
///
/// Forge applies its own broader installation-root policy: for example, a
/// safe read-only root or a root owned by uid 0 may be valid. Mason therefore
/// owns only creation here. A final entry that exists before this call, or
/// wins the final `mkdirat` race, must still pin as a real directory without
/// symlinks, but its inode and mode are left for Forge to validate unchanged.
#[cfg(test)]
pub(crate) fn prepare_missing_private_workspace_root(path: &Path) -> io::Result<PathBuf> {
    prepare_missing_private_workspace_root_pinned(path).map(|(path, _anchor)| path)
}

/// Create a missing Forge root and retain the exact selected directory.
///
/// Existing roots remain mode-for-mode unchanged so Forge can apply its wider
/// root-owned/read-only policy. The retained `O_PATH` descriptor is valid for
/// those roots even when Mason cannot open them for reading or writing.
pub(crate) fn prepare_missing_private_workspace_root_pinned(path: &Path) -> io::Result<(PathBuf, StdFile)> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    match std::fs::symlink_metadata(&absolute) {
        Ok(_) => {
            let anchor = pin_workspace_root(&absolute)?;
            return Ok((absolute, anchor));
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(source),
    }

    prepare_private_workspace_root_with_policy_pinned(&absolute, WorkspaceRootLeafPolicy::PreserveExisting)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceRootLeafPolicy {
    NormalizeExisting,
    PreserveExisting,
}

enum EnsuredPrivateDirectory {
    Controlled(StdFile),
    ExistingLeaf(StdFile),
}

#[cfg(test)]
fn prepare_private_workspace_root_with_policy(
    path: &Path,
    leaf_policy: WorkspaceRootLeafPolicy,
) -> io::Result<PathBuf> {
    prepare_private_workspace_root_with_policy_pinned(path, leaf_policy).map(|(path, _anchor)| path)
}

fn prepare_private_workspace_root_with_policy_pinned(
    path: &Path,
    leaf_policy: WorkspaceRootLeafPolicy,
) -> io::Result<(PathBuf, StdFile)> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let leaf = absolute
        .file_name()
        .ok_or_else(|| invalid_binding(format!("private workspace root has no leaf name: {path:?}")))?
        .to_owned();
    let mut ancestor = absolute
        .parent()
        .ok_or_else(|| invalid_binding(format!("private workspace root has no parent: {path:?}")))?
        .to_owned();
    let mut missing = vec![leaf];

    loop {
        match std::fs::symlink_metadata(&ancestor) {
            Ok(_) => break,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    invalid_binding(format!(
                        "cannot find an existing parent for private workspace root {path:?}"
                    ))
                })?;
                missing.push(name.to_owned());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| {
                        invalid_binding(format!(
                            "cannot find an existing parent for private workspace root {path:?}"
                        ))
                    })?
                    .to_owned();
            }
            Err(source) => return Err(source),
        }
    }

    // Parent aliases are resolved once, before the authoritative descriptor is
    // opened. The returned root path uses that canonical parent, so later
    // retained-anchor checks do not depend on the alias remaining unchanged.
    let ancestor = ancestor.canonicalize()?;
    let anchor = open_directory_nofollow(&ancestor)?;
    require_controlled_directory(&anchor, &ancestor, false)?;
    missing.reverse();
    let relative = missing.iter().collect::<PathBuf>();
    let root_path = ancestor.join(&relative);
    let root = match ensure_private_directory_at_with_policy(&anchor, &relative, &root_path, leaf_policy)? {
        EnsuredPrivateDirectory::Controlled(root) => {
            require_controlled_directory(&root, &root_path, true)?;
            let reopened = open_directory_nofollow(&root_path)?;
            require_same_directory(&root, &reopened, &root_path)?;
            root
        }
        EnsuredPrivateDirectory::ExistingLeaf(root) => {
            debug_assert_eq!(leaf_policy, WorkspaceRootLeafPolicy::PreserveExisting);
            let reopened = pin_workspace_root(&root_path)?;
            require_same_directory(&root, &reopened, &root_path)?;
            root
        }
    };
    Ok((root_path, root))
}

fn private_host_relative(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let relative = path.strip_prefix(root).map_err(|_| {
        invalid_binding(format!(
            "private host path {path:?} is not beneath workspace root {root:?}"
        ))
    })?;
    let raw = relative.as_os_str().as_bytes();
    if raw.is_empty()
        || raw.len() > MAX_PRIVATE_HOST_PATH_BYTES
        || raw.contains(&0)
        || relative.components().count() > MAX_PRIVATE_HOST_PATH_COMPONENTS
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_binding(format!(
            "invalid private host path beneath workspace root: {path:?}"
        )));
    }
    Ok(relative.to_owned())
}

fn ensure_private_directory_at(root: &StdFile, relative: &Path, display: &Path) -> io::Result<StdFile> {
    match ensure_private_directory_at_with_policy(root, relative, display, WorkspaceRootLeafPolicy::NormalizeExisting)?
    {
        EnsuredPrivateDirectory::Controlled(directory) => Ok(directory),
        EnsuredPrivateDirectory::ExistingLeaf(_) => {
            unreachable!("normalizing private-directory traversal cannot preserve an existing leaf")
        }
    }
}

fn ensure_private_directory_at_with_policy(
    root: &StdFile,
    relative: &Path,
    display: &Path,
    leaf_policy: WorkspaceRootLeafPolicy,
) -> io::Result<EnsuredPrivateDirectory> {
    let root_metadata = root.metadata()?;
    let mut current = root.try_clone()?;
    let mut traversed = PathBuf::new();
    let component_count = relative.components().count();

    for (index, component) in relative.components().enumerate() {
        let Component::Normal(name) = component else {
            return Err(invalid_binding(format!("invalid private host path: {display:?}")));
        };
        traversed.push(name);
        let name = CString::new(name.as_bytes())
            .map_err(|_| invalid_binding(format!("private host path contains NUL: {display:?}")))?;
        let leaf = index + 1 == component_count;

        if leaf && leaf_policy == WorkspaceRootLeafPolicy::PreserveExisting {
            if !mkdir_private_directory_at(&current, &name, &traversed)? {
                let existing = open_path_child(&current, &name).map_err(|source| {
                    io::Error::new(
                        source.kind(),
                        format!("pin existing private host leaf {traversed:?}: {source}"),
                    )
                })?;
                require_workspace_root_directory(&existing, display)?;
                return Ok(EnsuredPrivateDirectory::ExistingLeaf(existing));
            }
            current = recover_created_private_directory(&root_metadata, &current, &name, &traversed, display)?;
            continue;
        }

        let mut next = open_private_child(&current, &name);
        if next
            .as_ref()
            .is_err_and(|source| source.kind() == io::ErrorKind::NotFound)
        {
            let created = mkdir_private_directory_at(&current, &name, &traversed)?;
            if created {
                current = recover_created_private_directory(&root_metadata, &current, &name, &traversed, display)?;
                continue;
            }
            next = open_private_child(&current, &name);
        }
        let next = next.map_err(|source| {
            io::Error::new(
                source.kind(),
                format!(
                    "open private host directory component {traversed:?} without links or mount crossings: {source}"
                ),
            )
        })?;
        require_same_device(&root_metadata, &next, display)?;
        require_controlled_directory(&next, display, false)?;

        if leaf {
            next.set_permissions(std::fs::Permissions::from_mode(0o700))?;
            require_controlled_directory(&next, display, true)?;
        }
        current = next;
    }
    Ok(EnsuredPrivateDirectory::Controlled(current))
}

fn mkdir_private_directory_at(parent: &StdFile, name: &CStr, display: &Path) -> io::Result<bool> {
    loop {
        // SAFETY: `parent` and `name` remain live. mkdirat interprets one
        // validated normal component relative to the authenticated parent.
        if unsafe { nix::libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
            return Ok(true);
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::AlreadyExists => return Ok(false),
            _ => {
                return Err(io::Error::new(
                    source.kind(),
                    format!("create private host directory {display:?}: {source}"),
                ));
            }
        }
    }
}

fn recover_created_private_directory(
    root_metadata: &std::fs::Metadata,
    parent: &StdFile,
    name: &CStr,
    traversed: &Path,
    display: &Path,
) -> io::Result<StdFile> {
    let pinned = open_path_child(parent, name).map_err(|source| {
        io::Error::new(
            source.kind(),
            format!("pin newly-created private host directory {traversed:?}: {source}"),
        )
    })?;
    require_same_device(root_metadata, &pinned, display)?;
    require_created_private_directory(&pinned, traversed)?;
    chmod_path_descriptor(&pinned, 0o700)?;

    let directory = open_private_child(parent, name).map_err(|source| {
        io::Error::new(
            source.kind(),
            format!("reopen newly-created private host directory {traversed:?}: {source}"),
        )
    })?;
    require_same_device(root_metadata, &directory, display)?;
    require_same_directory(&pinned, &directory, display)?;
    require_controlled_directory(&directory, display, true)?;
    Ok(directory)
}

fn require_created_private_directory(directory: &StdFile, path: &Path) -> io::Result<()> {
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let owner = unsafe { nix::libc::geteuid() };
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir() || metadata.uid() != owner || mode & !0o700 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "new private host directory is not a safe owner-only residue: {path:?} (uid={}, mode={mode:#06o})",
                metadata.uid()
            ),
        ));
    }
    Ok(())
}

fn require_workspace_root_directory(directory: &StdFile, path: &Path) -> io::Result<()> {
    if directory.metadata()?.file_type().is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("workspace root is not a directory and will not be followed: {path:?}"),
        ))
    }
}

fn private_parent_and_leaf(
    root: &StdFile,
    relative: &Path,
    display: &Path,
    create_parent: bool,
) -> io::Result<(Option<StdFile>, CString)> {
    let mut components = relative.components().collect::<Vec<_>>();
    let Some(Component::Normal(leaf)) = components.pop() else {
        return Err(invalid_binding(format!("invalid private host leaf: {display:?}")));
    };
    let leaf = CString::new(leaf.as_bytes())
        .map_err(|_| invalid_binding(format!("private host leaf contains NUL: {display:?}")))?;
    if components.is_empty() {
        return Ok((Some(root.try_clone()?), leaf));
    }
    let parent_relative = components.iter().collect::<PathBuf>();
    if create_parent {
        let parent_display = display.parent().unwrap_or(display);
        return ensure_private_directory_at(root, &parent_relative, parent_display).map(|file| (Some(file), leaf));
    }

    let mut current = root.try_clone()?;
    for component in components {
        let Component::Normal(name) = component else {
            return Err(invalid_binding(format!("invalid private host parent: {display:?}")));
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| invalid_binding(format!("private host parent contains NUL: {display:?}")))?;
        current = match open_private_child(&current, &name) {
            Ok(next) => next,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok((None, leaf)),
            Err(source) => return Err(source),
        };
        require_controlled_directory(&current, display, false)?;
    }
    Ok((Some(current), leaf))
}

fn stale_leaf_name(leaf: &CStr) -> io::Result<CString> {
    let mut bytes = Vec::with_capacity(leaf.to_bytes().len() + 13);
    bytes.push(b'.');
    bytes.extend_from_slice(leaf.to_bytes());
    bytes.extend_from_slice(b".cast-stale");
    if bytes.len() > 255 {
        return Err(invalid_binding(
            "private host quarantine name exceeds NAME_MAX".to_owned(),
        ));
    }
    CString::new(bytes).map_err(|_| invalid_binding("private host quarantine name contains NUL".to_owned()))
}

fn create_private_leaf(parent: &StdFile, leaf: &CStr, display: &Path) -> io::Result<()> {
    // SAFETY: parent and leaf remain live and leaf is one normal component.
    if unsafe { nix::libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) } == -1 {
        let source = io::Error::last_os_error();
        return Err(io::Error::new(
            source.kind(),
            format!("create fresh private host directory {display:?}: {source}"),
        ));
    }
    let directory = open_controlled_named_directory(parent, leaf, display)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("new private host directory disappeared: {display:?}"),
        )
    })?;
    require_controlled_directory(&directory, display, true)
}

fn open_controlled_named_directory(parent: &StdFile, name: &CStr, display: &Path) -> io::Result<Option<StdFile>> {
    let pinned = match open_path_child(parent, name) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    let metadata = pinned.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "existing private host leaf is unsafe: {display:?} (uid={}, mode={:#06o})",
                metadata.uid(),
                metadata.mode() & 0o7777
            ),
        ));
    }
    chmod_path_descriptor(&pinned, 0o700)?;
    let directory = open_private_child(parent, name)?;
    if directory_identity(&pinned)? != directory_identity(&directory)? {
        return Err(io::Error::other(format!(
            "private host leaf was replaced while opening: {display:?}"
        )));
    }
    require_controlled_directory(&directory, display, true)?;
    Ok(Some(directory))
}

fn open_path_child(parent: &StdFile, name: &CStr) -> io::Result<StdFile> {
    // SAFETY: an all-zero open_how is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from((nix::libc::O_PATH | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC) as u32);
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV;
    // SAFETY: parent, component, and open_how remain live for the syscall.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(result)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {result}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    Ok(unsafe { StdFile::from_raw_fd(descriptor) })
}

fn rename_noreplace(parent: &StdFile, from: &CStr, to: &CStr, display: &Path) -> io::Result<()> {
    // SAFETY: both names and the shared parent descriptor remain live.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    if result == -1 {
        let source = io::Error::last_os_error();
        return Err(io::Error::new(
            source.kind(),
            format!("atomically detach private host directory {display:?}: {source}"),
        ));
    }
    Ok(())
}

fn directory_identity(file: &StdFile) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

const MAX_PURGE_ENTRIES: usize = 1_000_000;
const MAX_PURGE_OPERATIONS: usize = 2_000_000;
const MAX_PURGE_NAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_PURGE_DEPTH: usize = 128;
const PURGE_TIMEOUT: Duration = Duration::from_secs(300);

struct PurgeBudget {
    entries: usize,
    operations: usize,
    name_bytes: usize,
    deadline: Instant,
    device: u64,
}

impl PurgeBudget {
    fn new(root: &StdFile) -> io::Result<Self> {
        Ok(Self {
            entries: 0,
            operations: 0,
            name_bytes: 0,
            deadline: Instant::now() + PURGE_TIMEOUT,
            device: root.metadata()?.dev(),
        })
    }

    fn account(&mut self, name_bytes: usize, entry: bool) -> io::Result<()> {
        self.operations = self.operations.checked_add(1).ok_or_else(purge_limit_error)?;
        if entry {
            self.entries = self.entries.checked_add(1).ok_or_else(purge_limit_error)?;
            self.name_bytes = self.name_bytes.checked_add(name_bytes).ok_or_else(purge_limit_error)?;
        }
        if self.operations > MAX_PURGE_OPERATIONS
            || self.entries > MAX_PURGE_ENTRIES
            || self.name_bytes > MAX_PURGE_NAME_BYTES
            || Instant::now() > self.deadline
        {
            return Err(purge_limit_error());
        }
        Ok(())
    }
}

fn purge_limit_error() -> io::Error {
    io::Error::other("private host quarantine exceeds bounded cleanup limits")
}

fn purge_named_entry(
    parent: &StdFile,
    name: &CStr,
    budget: &mut PurgeBudget,
    depth: usize,
    display: &Path,
) -> io::Result<()> {
    budget.account(name.to_bytes().len(), false)?;
    let metadata = match metadata_at(parent, name) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(source),
    };
    if metadata.st_dev != budget.device {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            format!("private host quarantine crosses a mount: {display:?}"),
        ));
    }
    if metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFDIR {
        require_purge_depth(depth)?;
        let directory = open_directory_for_purge(parent, name, budget.device, display)?;
        for child in sorted_directory_names(&directory, budget)? {
            purge_named_entry(&directory, &child, budget, depth + 1, display)?;
        }
        budget.account(0, false)?;
        // SAFETY: parent and name remain live; AT_REMOVEDIR removes only this
        // now-empty directory and never follows a link.
        if unsafe { nix::libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), nix::libc::AT_REMOVEDIR) } == -1 {
            return Err(io::Error::last_os_error());
        }
    } else {
        budget.account(0, false)?;
        // SAFETY: unlinkat with flags 0 removes the named non-directory entry;
        // a symlink is removed rather than followed.
        if unsafe { nix::libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn require_purge_depth(depth: usize) -> io::Result<()> {
    if depth > MAX_PURGE_DEPTH {
        Err(purge_limit_error())
    } else {
        Ok(())
    }
}

fn metadata_at(parent: &StdFile, name: &CStr) -> io::Result<nix::libc::stat> {
    // SAFETY: all-zero stat is valid output storage and the arguments remain live.
    let mut metadata: nix::libc::stat = unsafe { zeroed() };
    // SAFETY: parent/name are valid and AT_SYMLINK_NOFOLLOW authenticates the
    // named entry itself.
    if unsafe {
        nix::libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut metadata,
            nix::libc::AT_SYMLINK_NOFOLLOW,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(metadata)
}

fn open_directory_for_purge(parent: &StdFile, name: &CStr, device: u64, display: &Path) -> io::Result<StdFile> {
    let pinned = open_path_child(parent, name)?;
    let metadata = pinned.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let owner = unsafe { nix::libc::geteuid() };
    if !metadata.file_type().is_dir() || metadata.uid() != owner || metadata.dev() != device {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("unsafe directory inside private host quarantine: {display:?}"),
        ));
    }
    // The detached root is exact 0700, so arbitrary build-produced descendant
    // modes are no longer reachable through a shared path. Normalize each
    // pinned owned directory only to make the bounded cleanup walk possible.
    chmod_path_descriptor(&pinned, 0o700)?;
    let directory = open_private_child(parent, name)?;
    if directory_identity(&pinned)? != directory_identity(&directory)? {
        return Err(io::Error::other(format!(
            "quarantine directory changed during cleanup: {display:?}"
        )));
    }
    Ok(directory)
}

fn sorted_directory_names(directory: &StdFile, budget: &mut PurgeBudget) -> io::Result<Vec<CString>> {
    let cursor = open_private_child(directory, c".")?;
    let raw = cursor.into_raw_fd();
    // SAFETY: fdopendir consumes this fresh descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(raw) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume the descriptor.
        unsafe { nix::libc::close(raw) };
        return Err(source);
    }
    let mut names = Vec::new();
    let result = (|| -> io::Result<()> {
        loop {
            // SAFETY: this process targets Linux and owns the DIR stream.
            unsafe { *nix::libc::__errno_location() = 0 };
            // SAFETY: stream remains valid until closed below.
            let entry = unsafe { nix::libc::readdir(stream) };
            if entry.is_null() {
                // SAFETY: errno is thread-local.
                let errno = unsafe { *nix::libc::__errno_location() };
                if errno != 0 {
                    return Err(io::Error::from_raw_os_error(errno));
                }
                break;
            }
            // SAFETY: d_name is NUL-terminated for the live dirent.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            budget.account(name.to_bytes().len(), true)?;
            names.push(name.to_owned());
        }
        Ok(())
    })();
    // SAFETY: closedir consumes and closes the descriptor held by stream.
    let close_result = unsafe { nix::libc::closedir(stream) };
    result?;
    if close_result == -1 {
        return Err(io::Error::last_os_error());
    }
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    Ok(names)
}

fn open_directory_nofollow(path: &Path) -> io::Result<StdFile> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
        .open(path)
}

/// Pin one workspace root without following any symlink in its pathname.
///
/// The returned `O_PATH` descriptor is suitable for descriptor-backed
/// container binds. No ownership or mode policy is imposed here because Forge
/// intentionally accepts safe root-owned and read-only resolver roots.
pub(crate) fn pin_workspace_root(path: &Path) -> io::Result<StdFile> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let encoded = CString::new(absolute.as_os_str().as_bytes())
        .map_err(|_| invalid_binding(format!("workspace root contains NUL: {absolute:?}")))?;
    // SAFETY: an all-zero open_how is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags =
        u64::from((nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC) as u32);
    how.resolve = (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64;
    let descriptor = loop {
        // SAFETY: the encoded pathname and open_how remain live. Success
        // returns one fresh descriptor owned below.
        let descriptor = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_openat2,
                nix::libc::AT_FDCWD,
                encoded.as_ptr(),
                &how,
                size_of::<nix::libc::open_how>(),
            )
        };
        if descriptor != -1 {
            break descriptor;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::new(
                source.kind(),
                format!("pin workspace root without symlinks {absolute:?}: {source}"),
            ));
        }
    };
    let descriptor = RawFd::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    let directory = unsafe { StdFile::from_raw_fd(descriptor) };
    require_workspace_root_directory(&directory, &absolute)?;
    Ok(directory)
}

/// Prove that a mutable workspace pathname still names one retained root.
///
/// The newly opened descriptor is used only as a name witness. Callers must
/// continue using `expected` as their authority so a substitution after this
/// check can never redirect a destructive operation to the replacement.
pub(crate) fn require_workspace_root_path(expected: &StdFile, path: &Path) -> io::Result<()> {
    require_workspace_root_directory(expected, path)?;
    let reopened = pin_workspace_root(path)?;
    require_same_directory(expected, &reopened, path)
}

fn open_private_child(parent: &StdFile, name: &CStr) -> io::Result<StdFile> {
    // SAFETY: an all-zero open_how is valid before its public fields are set.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(
        (nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NONBLOCK) as u32,
    );
    how.resolve = nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV;
    // SAFETY: parent, component, and open_how remain live for the syscall.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(result)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {result}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    Ok(unsafe { StdFile::from_raw_fd(descriptor) })
}

fn require_controlled_directory(directory: &StdFile, path: &Path, exact_private: bool) -> io::Result<()> {
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let owner = unsafe { nix::libc::geteuid() };
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != owner
        || metadata.mode() & 0o022 != 0
        || (exact_private && mode != 0o700)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "host directory is not privately controlled: {path:?} (uid={}, mode={mode:#06o})",
                metadata.uid()
            ),
        ));
    }
    Ok(())
}

fn require_same_device(root: &std::fs::Metadata, directory: &StdFile, path: &Path) -> io::Result<()> {
    let metadata = directory.metadata()?;
    if root.dev() != metadata.dev() {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            format!("private host path crosses a mount beneath the workspace: {path:?}"),
        ));
    }
    Ok(())
}

fn require_same_directory(expected: &StdFile, found: &StdFile, path: &Path) -> io::Result<()> {
    let expected = expected.metadata()?;
    let found = found.metadata()?;
    if expected.dev() != found.dev() || expected.ino() != found.ino() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("workspace path was replaced after construction: {path:?}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    };

    use super::*;
    use crate::package::test_derivation_plan;

    fn test_paths(root: &tempfile::TempDir, plan: &DerivationPlan) -> Paths {
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let output = root.path().join("output");
        util::ensure_dir_exists(&output).unwrap();
        Paths::new(&recipe, plan.layout.clone(), root.path(), output).unwrap()
    }

    #[test]
    fn preserve_existing_workspace_leaf_policy_pins_without_chmodding_directory_race_winner() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let forge = root.path().join("forge");
        std::fs::create_dir(&forge).unwrap();
        std::fs::set_permissions(&forge, std::fs::Permissions::from_mode(0o555)).unwrap();
        let before = std::fs::symlink_metadata(&forge).unwrap();

        let prepared =
            prepare_private_workspace_root_with_policy(&forge, WorkspaceRootLeafPolicy::PreserveExisting).unwrap();

        let after = std::fs::symlink_metadata(&forge).unwrap();
        assert_eq!(prepared, forge);
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(after.permissions().mode() & 0o7777, 0o555);
    }

    #[test]
    fn missing_workspace_root_rejects_existing_symlink_without_touching_target() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = root.path().join("unrelated");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"untouched").unwrap();
        let target_before = std::fs::symlink_metadata(&target).unwrap();
        let forge = root.path().join("forge");
        symlink(&target, &forge).unwrap();

        let error = prepare_missing_private_workspace_root(&forge).unwrap_err();

        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert!(std::fs::symlink_metadata(&forge).unwrap().file_type().is_symlink());
        let target_after = std::fs::symlink_metadata(&target).unwrap();
        assert_eq!(
            (target_after.dev(), target_after.ino()),
            (target_before.dev(), target_before.ino())
        );
        assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"untouched");
    }

    #[test]
    fn preserve_existing_workspace_leaf_policy_rejects_non_directory_race_winners_unchanged() {
        for kind in ["symlink", "file"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let target = root.path().join("unrelated");
            std::fs::create_dir(&target).unwrap();
            std::fs::write(target.join("keep"), b"untouched").unwrap();
            let forge = root.path().join("forge");
            match kind {
                "symlink" => symlink(&target, &forge).unwrap(),
                "file" => std::fs::write(&forge, b"not a directory").unwrap(),
                _ => unreachable!(),
            }
            let before = std::fs::symlink_metadata(&forge).unwrap();

            let error = prepare_private_workspace_root_with_policy(&forge, WorkspaceRootLeafPolicy::PreserveExisting)
                .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{kind}");
            let after = std::fs::symlink_metadata(&forge).unwrap();
            assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()), "{kind}");
            assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"untouched", "{kind}");
            if kind == "file" {
                assert_eq!(std::fs::read(&forge).unwrap(), b"not a directory");
            } else {
                assert_eq!(std::fs::read_link(&forge).unwrap(), target);
            }
        }
    }

    #[test]
    fn frozen_workspaces_are_keyed_by_the_complete_derivation_id() {
        let root = tempfile::tempdir().unwrap();
        let first_plan = test_derivation_plan();
        let mut second_plan = first_plan.clone();
        second_plan.source_date_epoch += 1;
        second_plan.validate().unwrap();
        assert_eq!(first_plan.package, second_plan.package);
        assert_ne!(first_plan.derivation_id(), second_plan.derivation_id());

        let mut first = test_paths(&root, &first_plan);
        let mut second = test_paths(&root, &second_plan);
        first.bind_to_plan(&first_plan).unwrap();
        second.bind_to_plan(&second_plan).unwrap();

        assert!(!first.rootfs().host.exists());
        assert!(!second.rootfs().host.exists());
        assert!(first.rootfs().host.parent().unwrap().is_dir());
        assert_ne!(first.rootfs().host, second.rootfs().host);
        assert_ne!(first.build().host, second.build().host);
        assert_ne!(first.artefacts().host, second.artefacts().host);
        assert_eq!(
            first.rootfs().host.file_name().and_then(|name| name.to_str()),
            Some(first_plan.derivation_id().as_str())
        );
        assert_eq!(
            second.rootfs().host.file_name().and_then(|name| name.to_str()),
            Some(second_plan.derivation_id().as_str())
        );
        first.require_plan(&first_plan).unwrap();
        assert!(first.require_plan(&second_plan).is_err());
    }

    #[test]
    fn paths_remain_recipe_keyed_until_the_frozen_plan_is_bound() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let paths = test_paths(&root, &plan);

        assert!(matches!(&paths.id, Id::Recipe(_)));
        assert!(paths.require_plan(&plan).is_err());
    }

    #[test]
    fn invalid_recipe_identity_is_rejected_before_host_paths_are_created() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        recipe.declaration.meta.pname = "/tmp/cast-path-escape".to_owned();
        let output = root.path().join("output");
        util::ensure_dir_exists(&output).unwrap();

        let error = Paths::new(&recipe, plan.layout, root.path(), output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!root.path().join("root").exists());
    }

    #[test]
    #[allow(deprecated)]
    fn execution_guard_exclusively_locks_the_derivation_path() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut paths = test_paths(&root, &plan);
        paths.bind_to_plan(&plan).unwrap();

        let guard = paths.acquire_execution_lock(&plan).unwrap();
        assert_eq!(guard.path(), paths.execution_lock_path(&plan).unwrap());
        let contender = File::options()
            .create(true)
            .write(true)
            .truncate(false)
            .open(guard.path())
            .unwrap();
        assert_eq!(
            flock(contender.as_raw_fd(), FlockArg::LockExclusiveNonblock),
            Err(nix::errno::Errno::EWOULDBLOCK)
        );

        drop(guard);
        flock(contender.as_raw_fd(), FlockArg::LockExclusiveNonblock).unwrap();
    }

    #[test]
    fn execution_lock_name_accepts_exact_name_max_and_rejects_name_max_plus_one() {
        let maximum_id = MAX_EXECUTION_LOCK_NAME_BYTES - EXECUTION_LOCK_SUFFIX.len();
        let accepted = execution_lock_leaf(&"a".repeat(maximum_id)).unwrap();
        assert_eq!(accepted.to_bytes().len(), MAX_EXECUTION_LOCK_NAME_BYTES);

        let error = execution_lock_leaf(&"a".repeat(maximum_id + 1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(execution_lock_leaf("").is_err());
        assert!(execution_lock_leaf("not/a-component").is_err());
        assert!(execution_lock_leaf("not\0a-component").is_err());
    }

    #[test]
    fn execution_lock_rejects_fifo_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut paths = test_paths(&root, &plan);
        paths.bind_to_plan(&plan).unwrap();
        let lock_path = paths.execution_lock_path(&plan).unwrap();
        let lock_path_c = CString::new(lock_path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the test path is one live NUL-terminated string.
        assert_eq!(unsafe { nix::libc::mkfifo(lock_path_c.as_ptr(), 0o600) }, 0);

        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            send.send(paths.acquire_execution_lock(&plan).map(drop)).unwrap();
        });
        let error = receive
            .recv_timeout(Duration::from_secs(2))
            .expect("O_NONBLOCK must prevent a hostile FIFO from hanging lock acquisition")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn execution_lock_rejects_symlink_and_multiple_link_regular_file() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut paths = test_paths(&root, &plan);
        paths.bind_to_plan(&plan).unwrap();
        let lock_path = paths.execution_lock_path(&plan).unwrap();
        let outside = root.path().join("outside-lock");
        std::fs::write(&outside, b"outside").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&outside, &lock_path).unwrap();

        assert!(paths.acquire_execution_lock(&plan).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");

        std::fs::remove_file(&lock_path).unwrap();
        std::fs::write(&lock_path, b"").unwrap();
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let alias = root.path().join("lock-alias");
        std::fs::hard_link(&lock_path, &alias).unwrap();
        let error = paths.acquire_execution_lock(&plan).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::metadata(&lock_path).unwrap().nlink(), 2);
    }

    #[test]
    fn execution_lock_path_replacement_invalidates_guard_and_cannot_overlap_a_second_guard() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut paths = test_paths(&root, &plan);
        paths.bind_to_plan(&plan).unwrap();
        let guard = paths.acquire_execution_lock(&plan).unwrap();
        let lock_path = guard.path().to_owned();

        std::fs::remove_file(&lock_path).unwrap();
        std::fs::write(&lock_path, b"replacement").unwrap();
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(paths.require_execution_lock(&guard, &plan).is_err());

        let contender_paths = paths.clone();
        let contender_plan = plan.clone();
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            send.send(contender_paths.acquire_execution_lock(&contender_plan))
                .unwrap();
        });
        assert!(
            matches!(
                receive.recv_timeout(Duration::from_millis(100)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "the stable workspace gate must prevent overlapping guards after pathname replacement"
        );

        drop(guard);
        let replacement_guard = receive
            .recv_timeout(Duration::from_secs(2))
            .expect("the contender must proceed after the original stable gate is released")
            .unwrap();
        paths.require_execution_lock(&replacement_guard, &plan).unwrap();
    }

    #[test]
    fn frozen_scratch_is_atomically_replaced_and_bounded_cleanup_never_follows_links() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut paths = test_paths(&root, &plan);
        paths.bind_to_plan(&plan).unwrap();
        let scratch = paths.artefacts().host;
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"outside").unwrap();

        let leaf = CString::new(scratch.file_name().unwrap().as_bytes()).unwrap();
        let stale = stale_leaf_name(&leaf).unwrap();
        let stale_path = scratch.parent().unwrap().join(OsStr::from_bytes(stale.to_bytes()));
        std::fs::create_dir(&stale_path).unwrap();
        std::fs::set_permissions(&stale_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(stale_path.join("interrupted-retry"), b"stale").unwrap();
        symlink(&outside, stale_path.join("outside-link")).unwrap();

        let first = paths.prepare_fresh_private_host_directory(&scratch).unwrap();
        assert!(!stale_path.exists());
        assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
        let first_identity = directory_identity(&first).unwrap();
        std::fs::create_dir(scratch.join("nested")).unwrap();
        std::fs::write(scratch.join("nested/file"), b"stale").unwrap();
        symlink(&outside, scratch.join("nested/outside-link")).unwrap();

        let second = paths.prepare_fresh_private_host_directory(&scratch).unwrap();
        assert_ne!(first_identity, directory_identity(&second).unwrap());
        assert!(std::fs::read_dir(&scratch).unwrap().next().is_none());
        assert_eq!(
            std::fs::metadata(&scratch).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
        assert!(!stale_path.exists());

        paths.remove_private_host_directory(&scratch).unwrap();
        assert!(!scratch.exists());
        paths.remove_private_host_directory(&scratch).unwrap();
    }

    #[test]
    fn frozen_scratch_rejects_unsafe_existing_leaf_without_renaming_or_chmoding_it() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut paths = test_paths(&root, &plan);
        paths.bind_to_plan(&plan).unwrap();
        let scratch = paths.build().host;
        std::fs::create_dir(&scratch).unwrap();
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o770)).unwrap();
        std::fs::write(scratch.join("must-survive"), b"unsafe").unwrap();

        let error = paths.prepare_fresh_private_host_directory(&scratch).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            std::fs::metadata(&scratch).unwrap().permissions().mode() & 0o7777,
            0o770
        );
        assert_eq!(std::fs::read(scratch.join("must-survive")).unwrap(), b"unsafe");
    }

    #[test]
    fn frozen_private_source_rejects_a_symlinked_parent_without_touching_its_target() {
        let root = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let mut paths = test_paths(&root, &plan);
        paths.bind_to_plan(&plan).unwrap();
        let outside = root.path().join("outside-cache");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, root.path().join("derivations")).unwrap();
        let cache = paths.derivation_cache_host(plan.derivation_id().as_str(), "ccache");

        assert!(paths.prepare_private_host_directory(&cache).is_err());
        assert!(std::fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn retained_workspace_descriptor_detects_path_substitution() {
        let outer = tempfile::tempdir().unwrap();
        let workspace = outer.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700)).unwrap();
        let plan = test_derivation_plan();
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let output = outer.path().join("output");
        std::fs::create_dir(&output).unwrap();
        let paths = Paths::new(&recipe, plan.layout, &workspace, output).unwrap();

        std::fs::rename(&workspace, outer.path().join("detached-workspace")).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(paths.frozen_workspace_anchor().is_err());
    }

    #[test]
    fn purge_budgets_accept_each_exact_boundary_and_reject_n_plus_one() {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut entries = PurgeBudget {
            entries: MAX_PURGE_ENTRIES - 1,
            operations: 0,
            name_bytes: 0,
            deadline,
            device: 0,
        };
        entries.account(0, true).unwrap();
        assert_eq!(entries.entries, MAX_PURGE_ENTRIES);
        assert!(entries.account(0, true).is_err());

        let mut operations = PurgeBudget {
            entries: 0,
            operations: MAX_PURGE_OPERATIONS - 1,
            name_bytes: 0,
            deadline,
            device: 0,
        };
        operations.account(0, false).unwrap();
        assert_eq!(operations.operations, MAX_PURGE_OPERATIONS);
        assert!(operations.account(0, false).is_err());

        let mut names = PurgeBudget {
            entries: 0,
            operations: 0,
            name_bytes: MAX_PURGE_NAME_BYTES - 1,
            deadline,
            device: 0,
        };
        names.account(1, true).unwrap();
        assert_eq!(names.name_bytes, MAX_PURGE_NAME_BYTES);
        assert!(names.account(1, true).is_err());

        require_purge_depth(MAX_PURGE_DEPTH).unwrap();
        assert!(require_purge_depth(MAX_PURGE_DEPTH + 1).is_err());
    }
}
