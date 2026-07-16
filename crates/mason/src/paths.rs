// SPDX-FileCopyrightText: 2024 AerynOS Developers

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
        let started = Instant::now();
        let deadline = started
            .checked_add(EXECUTION_LOCK_WAIT_TIMEOUT)
            .ok_or_else(|| io::Error::other("execution lock deadline overflowed"))?;
        self.acquire_execution_lock_until(plan, deadline, Instant::now, std::thread::sleep)
    }

    fn acquire_execution_lock_until<N, W>(
        &self,
        plan: &DerivationPlan,
        deadline: Instant,
        mut now: N,
        mut wait: W,
    ) -> io::Result<ExecutionLock>
    where
        N: FnMut() -> Instant,
        W: FnMut(Duration),
    {
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
        lock_exclusive_until(
            &workspace_gate,
            "workspace execution gate",
            &self.host_root,
            deadline,
            &mut now,
            &mut wait,
        )?;
        self.revalidate_host_root()?;

        let lock_dir = self.prepare_private_host_directory(&self.execution_lock_dir())?;
        let root_metadata = self.host_root_anchor.metadata()?;
        require_same_device(&root_metadata, &lock_dir, &path)?;
        let lock_dir_identity = directory_identity(&lock_dir)?;

        let file = open_or_create_execution_lock_file(&lock_dir, &leaf)?;
        let file_identity = require_controlled_lock_file(&file, &root_metadata, &path)?;
        lock_exclusive_until(&file, "derivation execution lock", &path, deadline, &mut now, &mut wait)?;
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

fn lock_exclusive_until<N, W>(
    file: &impl AsRawFd,
    description: &str,
    path: &Path,
    deadline: Instant,
    now: &mut N,
    wait: &mut W,
) -> io::Result<()>
where
    N: FnMut() -> Instant,
    W: FnMut(Duration),
{
    lock_exclusive_until_with(description, path, deadline, now, wait, || try_lock_exclusive(file))
}

fn lock_exclusive_until_with<N, W, L>(
    description: &str,
    path: &Path,
    deadline: Instant,
    now: &mut N,
    wait: &mut W,
    mut try_lock: L,
) -> io::Result<()>
where
    N: FnMut() -> Instant,
    W: FnMut(Duration),
    L: FnMut() -> io::Result<bool>,
{
    loop {
        // A lock which is immediately available exactly at the deadline may
        // still be acquired once. A clock already past the absolute deadline
        // must not issue another flock, including the first attempt for the
        // second lock or a retry after an overslept polling interval.
        if now() > deadline {
            return Err(execution_lock_timeout(description, path));
        }
        match try_lock() {
            Ok(true) => return Ok(()),
            Ok(false) => {
                let current = now();
                if current >= deadline {
                    return Err(execution_lock_timeout(description, path));
                }
                wait(EXECUTION_LOCK_RETRY_INTERVAL.min(deadline.saturating_duration_since(current)));
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {
                if now() >= deadline {
                    return Err(execution_lock_timeout(description, path));
                }
            }
            Err(source) => return Err(source),
        }
    }
}

#[allow(deprecated)]
fn try_lock_exclusive(file: &impl AsRawFd) -> io::Result<bool> {
    match flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock) {
        Ok(()) => Ok(true),
        Err(error) => {
            let source = errno_to_io(error);
            if source.kind() == io::ErrorKind::WouldBlock {
                Ok(false)
            } else {
                Err(source)
            }
        }
    }
}

fn execution_lock_timeout(description: &str, path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("timed out waiting for {description} at {path:?}"),
    )
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
const EXECUTION_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);
// There is no single whole-derivation wall-time ceiling: this guard spans
// setup, every build step, analysis, publication, and cleanup. Use the
// executor's longest authoritative single-step ceiling as a conservative
// contention bound. It also exceeds the two-hour delegated-fixture runtime,
// without pretending that waiting can cover the theoretical maximum of every
// admitted step in one derivation.
const EXECUTION_LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

include!("paths/execution_lock_files.rs");
include!("paths/workspace_preparation.rs");
include!("paths/bounded_cleanup.rs");
include!("paths/workspace_identity.rs");

#[cfg(test)]
mod tests;
